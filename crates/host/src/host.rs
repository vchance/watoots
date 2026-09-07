//! The engine, the policy, and loading plugins under it.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use sha2::{Digest, Sha256};
use wasmtime::component::types::ComponentItem;
use wasmtime::component::{Component, Type, Val};
use wasmtime::{Config, Engine};

use crate::audit::{AuditEvent, AuditHook, Verdict};
use crate::imports::{self, GrantReport, ImportDecision};
use crate::manifest::{LogLevel, Manifest};
use crate::plugin::{Plugin, Wiring};
use crate::profile::Profiling;
use crate::signature::{self, TrustedKey};
use crate::trace::TraceHook;
use crate::{Error, ErrorKind, Result};

/// How often the epoch ticker advances the engine's epoch.
///
/// Deadlines are expressed in whole ticks, so this is also the granularity of a
/// `timeout`: a 200ms timeout is 200 ticks.
const EPOCH_TICK: Duration = Duration::from_millis(1);

/// How many epoch ticks a duration is worth, rounded up and never zero.
///
/// A deadline of zero ticks has already expired, so anything shorter than one
/// tick becomes one tick: that is the granularity `EPOCH_TICK` can express, and
/// asking for less would trap immediately rather than sooner.
pub(crate) fn epoch_ticks(duration: Duration) -> u64 {
    let tick = EPOCH_TICK.as_nanos();
    u64::try_from(duration.as_nanos().div_ceil(tick))
        .unwrap_or(u64::MAX)
        .max(1)
}

/// A function the application serves to plugins.
///
/// Dynamically typed for the same reason [`Plugin::call`] is: it is the shape
/// the C API can express, and the shape a recorder can serialize without
/// knowing the world ahead of time.
pub type HostFunc = Arc<dyn Fn(&HostCall<'_>) -> Result<Vec<Val>> + Send + Sync>;

/// One in-flight call from a plugin into the application.
///
/// Carries the call's own result types as well as its arguments. A statically
/// typed host does not need them, but a dynamic one does: a caller working in
/// text — the C API, the CLI — has to know whether `42` should come back as a
/// `u8` or an `s64` before it can answer.
pub struct HostCall<'a> {
    pub(crate) args: &'a [Val],
    pub(crate) result_types: &'a [Type],
}

impl<'a> HostCall<'a> {
    /// The arguments the guest passed.
    #[must_use]
    pub fn args(&self) -> &'a [Val] {
        self.args
    }

    /// The types this function must return, from the world it was declared in.
    #[must_use]
    pub fn result_types(&self) -> &'a [Type] {
        self.result_types
    }
}

/// Where a plugin's `wasi:logging` messages go.
///
/// The application supplies this; watoots wires the interface and stays out of
/// the logging-framework business. It is invoked inline on the guest's call, so
/// it must be cheap and must not panic — and, like a [`HostFunc`], it may be
/// entered from any thread.
pub type LogSink = Arc<dyn Fn(&LogRecord<'_>) + Send + Sync>;

/// One message a plugin emitted through `wasi:logging`.
///
/// There is deliberately **no timestamp field**. A guest-supplied one would
/// defeat the pinned wall clock and make a recording unreplayable, and a
/// host-supplied one would be redundant: every logging framework a sink could
/// forward to stamps its own records, at an instant nearer the truth than the
/// moment the canonical ABI finished copying the string out of guest memory.
/// So the sink stamps it, from the host clock, by doing nothing at all.
#[derive(Debug, Clone, Copy)]
pub struct LogRecord<'a> {
    pub(crate) level: LogLevel,
    pub(crate) context: &'a str,
    pub(crate) message: &'a str,
}

impl<'a> LogRecord<'a> {
    /// Severity, already checked against the manifest's ceiling: a record
    /// reaching the sink is one the manifest admits.
    #[must_use]
    pub fn level(&self) -> LogLevel {
        self.level
    }

    /// The guest's uninterpreted grouping string. Untrusted, and not
    /// necessarily short — treat it as data, never as a format string.
    #[must_use]
    pub fn context(&self) -> &'a str {
        self.context
    }

    /// The message text. Untrusted, same caveat.
    #[must_use]
    pub fn message(&self) -> &'a str {
        self.message
    }
}

impl fmt::Debug for HostCall<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostCall")
            .field("args", &self.args)
            .field("results", &self.result_types.len())
            .finish()
    }
}

/// A configured engine plus the policy every plugin loaded from it runs under.
///
/// Cloning a `Host` shares the engine, its compiled-code cache, and the epoch
/// ticker.
///
/// # Running many plugins
///
/// One host serves any number of plugins, including many instances of the same
/// component: [`Host::load`] takes `&self`, and each [`Plugin`] it returns owns
/// a separate `Store`, so instances share compiled code and share no state.
/// Two instances of the same component cannot see each other's memory, globals
/// or tables, and a trap in one does not touch the other. A [`Registry`] is the
/// same thing with names attached, and it refuses a duplicate *name* — two
/// instances of one component are ordinary, two plugins called `lint` are a
/// mistake worth an error.
///
/// A `Host` is `Send + Sync` and a `Plugin` is `Send` but not `Sync`, so the
/// threading model is: share the host, move a plugin to the thread that will
/// call it. A plugin reachable from two threads at once would need a lock
/// anyway — `Plugin::call` takes `&mut self` because a component instance is a
/// single thread of execution.
///
/// [`Registry`]: crate::Registry
#[derive(Clone)]
pub struct Host {
    inner: Arc<HostInner>,
}

struct HostInner {
    engine: Engine,
    manifest: Manifest,
    /// `manifest.signature.keys`, parsed once.
    ///
    /// Parsed at build time rather than per load: a key that will not parse is
    /// a broken manifest, and discovering that on the first plugin would report
    /// a configuration mistake as if the plugin were at fault.
    trusted_keys: Vec<TrustedKey>,
    host_provided: BTreeSet<String>,
    host_funcs: BTreeMap<String, BTreeMap<String, HostFunc>>,
    vars: BTreeMap<String, String>,
    cache_dir: Option<PathBuf>,
    /// Components this host has already compiled, by cache key.
    ///
    /// Loading the same component twice is the normal shape of a plugin host —
    /// one instance per document, per tab, per worker — and each instance needs
    /// its own `Store`, not its own compilation. `Component` is `Arc`-backed, so
    /// a hit here costs a clone; a miss with a cache directory costs a
    /// `deserialize_file`; a miss without one costs a full compile, which for an
    /// 18MB interpreter guest is seconds.
    ///
    /// The key is a content hash, so a host that loads one plugin a thousand
    /// times holds one entry. Distinct components are the case that needs a
    /// bound: a host that reloads on every file change sees new bytes every
    /// time, and without eviction a morning's editing would accumulate every
    /// build it ever compiled. Hence [`COMPILED_CACHE_CAPACITY`] and
    /// `insertion_order`.
    compiled: RwLock<CompiledCache>,
    /// How many components this host actually built, as opposed to reused.
    compiles: AtomicU64,
    trace: Option<Arc<dyn TraceHook>>,
    audit: Option<Arc<dyn AuditHook>>,
    log_sink: Option<LogSink>,
    profiling: Option<Profiling>,
    /// Kept alive for as long as the host is; dropping it stops the thread.
    _ticker: Option<EpochTicker>,
}

/// Whether bytes are arriving as a new plugin or as a replacement for one.
///
/// [`Host::instantiate_plugin`] is the single path from bytes to a running
/// plugin, so a reload comes through it too — and the two are different
/// decisions to an auditor. This is the only thing that distinguishes them; it
/// changes nothing about what is checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LoadKind {
    /// A plugin that was not running before.
    Load,
    /// New bytes for a plugin that is. The success event is emitted by
    /// [`Plugin::reload`], once the replacement has actually taken over.
    Reload,
}

/// How many distinct components a host keeps compiled code for.
///
/// Sized for the case this cache exists to serve — a host with a handful of
/// plugins, instantiated many times each — rather than for the case that needs
/// the bound. Every realistic plugin set fits, and a host that has genuinely
/// churned through this many distinct components has told us the oldest are
/// not coming back.
const COMPILED_CACHE_CAPACITY: usize = 32;

/// Compiled components, keyed by content hash, bounded and evicted oldest-first.
///
/// Not an LRU: a load is not a use worth reordering the queue for, and reading
/// under a shared lock is worth more than eviction precision. The distinction
/// only matters once a host exceeds the capacity, which is already the unusual
/// case this exists to survive rather than to optimise.
#[derive(Default)]
struct CompiledCache {
    by_key: BTreeMap<String, Component>,
    insertion_order: VecDeque<String>,
}

impl CompiledCache {
    fn get(&self, key: &str) -> Option<&Component> {
        self.by_key.get(key)
    }

    fn insert(&mut self, key: String, component: Component) {
        if self.by_key.insert(key.clone(), component).is_some() {
            // Already present and already queued: two threads compiled the same
            // bytes at once. Queueing the key twice would evict a live entry
            // early and leave a dangling name behind it.
            return;
        }
        self.insertion_order.push_back(key);
        while self.insertion_order.len() > COMPILED_CACHE_CAPACITY {
            if let Some(oldest) = self.insertion_order.pop_front() {
                self.by_key.remove(&oldest);
            }
        }
    }
}

/// The threading model in `Host`'s documentation, enforced rather than
/// described: a shared host, and a plugin that moves to one thread.
///
/// `Plugin: !Sync` is not an oversight to be fixed later — it falls out of
/// `wasmtime_wasi`'s stream trait objects — so a change here is a change to
/// what callers were told, and should be deliberate.
const _: () = {
    const fn require_send_sync<T: Send + Sync>() {}
    const fn require_send<T: Send>() {}
    require_send_sync::<Host>();
    require_send::<crate::Plugin>();
};

impl Host {
    /// Start building a host.
    #[must_use]
    pub fn builder() -> HostBuilder {
        HostBuilder::default()
    }

    /// The engine every plugin is compiled and instantiated with.
    #[must_use]
    pub fn engine(&self) -> &Engine {
        &self.inner.engine
    }

    /// The policy in force.
    #[must_use]
    pub fn manifest(&self) -> &Manifest {
        &self.inner.manifest
    }

    /// Classify a component's imports against the manifest without loading it.
    ///
    /// This is the whole check as a pure question: it compiles the component
    /// but instantiates nothing, so it is safe to run over an untrusted plugin
    /// to show a user what it would be granted.
    pub fn inspect(&self, wasm: &[u8]) -> Result<GrantReport> {
        let component = self.compile(wasm)?;
        Ok(self.report_for(&component))
    }

    /// Check that a component implements a world.
    ///
    /// [`Host::inspect`] answers "does this plugin ask for anything it should
    /// not?" by intersecting its *imports* with the manifest. This answers the
    /// other half — "does it provide what I am about to call?" — by checking
    /// its *exports* against a world. A component can pass one and fail the
    /// other, and until now only the first was possible to ask.
    ///
    /// `wit` is a WIT file, a directory containing one (with an optional
    /// `deps/`), or a wasm-encoded WIT package. `world` names the world to
    /// check against, and may be omitted when the package declares exactly
    /// one. Both are paths the *application* chose, read by the host process:
    /// this is not a guest capability and does not touch the sandbox.
    ///
    /// The check itself is `wit_component::targets`, not our own
    /// reimplementation of conformance — see ADR-0007.
    pub fn check_targets(
        &self,
        wasm: &[u8],
        wit: impl AsRef<Path>,
        world: Option<&str>,
    ) -> Result<()> {
        let wit = wit.as_ref();
        let mut resolve = wit_parser::Resolve::default();
        let (package, _sources) = resolve.push_path(wit).map_err(|err| {
            Error::new(
                ErrorKind::InvalidArgument,
                format!("{}: {err:#}", wit.display()),
            )
        })?;

        let world_id = resolve.select_world(&[package], world).map_err(|err| {
            Error::new(
                ErrorKind::NotFound,
                match world {
                    Some(name) => format!("{}: no world {name:?}: {err:#}", wit.display()),
                    None => format!("{}: {err:#}", wit.display()),
                },
            )
        })?;

        // `targets` reports conformance failure by building a component that
        // imports the world and instantiating the candidate into it, so a
        // mismatch surfaces as a validation error. `Load` rather than a new
        // kind: the C enum is 1:1 with `ErrorKind` and adding a case after
        // 0.1.0 would move the others.
        wit_component::targets(&resolve, world_id, wasm).map_err(|err| {
            let detail = format!("{err:#}");
            // The first thing everyone hits. `targets` requires the world to
            // declare *every* import the component has, and a wasm32-wasip2
            // guest links wasi:io, wasi:cli and friends through std whether or
            // not its author asked for them -- so a hand-written application
            // world fails against a real guest until it includes WASI. Say so,
            // rather than leaving "missing import named `wasi:io/poll`" to be
            // interpreted.
            let hint = if detail.contains("missing import named `wasi:") {
                "\nnote: a world passed to --targets must declare the WASI \
                 imports the guest's toolchain links, not only the interfaces \
                 its author wrote; add `include wasi:cli/imports@0.2.x;` to the \
                 world, with the wasi WIT package vendored under deps/"
            } else {
                ""
            };
            Error::new(
                ErrorKind::Load,
                format!(
                    "component does not implement world {:?}: {detail}{hint}",
                    resolve.worlds[world_id].name
                ),
            )
        })
    }

    /// Every function a component imports, by interface and name.
    ///
    /// Compiles but does not instantiate, and does not consult the manifest:
    /// this answers "what does this plugin expect a host to provide", which is
    /// what you need before you can decide whether you can provide it. Replay
    /// uses it to serve every declared import, including ones a recording never
    /// happened to exercise.
    pub fn import_functions(&self, wasm: &[u8]) -> Result<Vec<ImportedFunction>> {
        let component = self.compile(wasm)?;
        let engine = &self.inner.engine;
        let mut found = Vec::new();

        for (interface, item) in component.component_type().imports(engine) {
            let ComponentItem::ComponentInstance(instance) = &item.ty else {
                continue;
            };
            for (func, nested) in instance.exports(engine) {
                if matches!(nested.ty, ComponentItem::ComponentFunc(_)) {
                    found.push(ImportedFunction {
                        interface: interface.to_string(),
                        func: func.to_string(),
                    });
                }
            }
        }
        Ok(found)
    }

    /// Every function a component exports, by name.
    ///
    /// The mirror of [`Host::import_functions`], and the same shape of answer:
    /// it compiles but does not instantiate, so it is safe to ask about an
    /// untrusted plugin. Only top-level exports are listed, because those are
    /// the ones [`Plugin::call`] can reach.
    ///
    /// `watoots fuzz` needs this to know what there is to call before it has a
    /// plugin to call it on.
    pub fn export_functions(&self, wasm: &[u8]) -> Result<Vec<String>> {
        let component = self.compile(wasm)?;
        let engine = &self.inner.engine;
        Ok(component
            .component_type()
            .exports(engine)
            .filter(|(_, item)| matches!(item.ty, ComponentItem::ComponentFunc(_)))
            .map(|(name, _)| name.to_string())
            .collect())
    }

    /// Load a component from disk.
    ///
    /// `${plugin_dir}` expands to the directory the component was loaded from,
    /// on top of any variables set on the builder.
    ///
    /// If the manifest lists `signature.keys`, the signature is read from
    /// `<path>.sig` — the file `cosign sign-blob --output-signature` writes —
    /// and a missing or unverifiable one refuses the load. With no keys
    /// configured no signature is looked for.
    pub fn load(&self, path: impl AsRef<Path>) -> Result<Plugin> {
        let path = path.as_ref();
        let wasm = read_component(path)?;
        let signature = self.read_signature_beside(path)?;
        let name = path.file_stem().map_or_else(
            || path.display().to_string(),
            |s| s.to_string_lossy().into(),
        );
        self.instantiate_plugin(
            &name,
            &wasm,
            plugin_dir_var(path),
            LoadKind::Load,
            signature.as_deref(),
        )
    }

    /// Load a component already in memory.
    ///
    /// `${plugin_dir}` is not defined for this path — there is no directory to
    /// point it at — so a manifest using it must be loaded with [`Host::load`].
    ///
    /// There is no file to read a signature from either, so under a manifest
    /// with `signature.keys` this refuses; use [`Host::load_binary_signed`].
    pub fn load_binary(&self, name: &str, wasm: &[u8]) -> Result<Plugin> {
        self.instantiate_plugin(name, wasm, BTreeMap::new(), LoadKind::Load, None)
    }

    /// Load a component from memory, with the signature that vouches for it.
    ///
    /// `signature` is base64, as `cosign sign-blob --output-signature` writes
    /// it. Verified against `signature.keys` before the component is compiled;
    /// with no keys configured the argument is ignored rather than rejected, so
    /// an application can pass one unconditionally and let the manifest decide
    /// whether it matters.
    pub fn load_binary_signed(&self, name: &str, wasm: &[u8], signature: &[u8]) -> Result<Plugin> {
        self.instantiate_plugin(name, wasm, BTreeMap::new(), LoadKind::Load, Some(signature))
    }

    /// Read `<path>.sig`, when the manifest gives us a reason to want one.
    ///
    /// Absent when no keys are configured, so a host that does not verify never
    /// touches the filesystem looking for a file that need not exist. A missing
    /// file when keys *are* configured is not reported here — it becomes the
    /// "no signature was supplied" refusal, which says what to do about it.
    pub(crate) fn read_signature_beside(&self, path: &Path) -> Result<Option<Vec<u8>>> {
        if self.inner.trusted_keys.is_empty() {
            return Ok(None);
        }
        let mut sig_path = path.as_os_str().to_os_string();
        sig_path.push(".sig");
        match std::fs::read(PathBuf::from(sig_path)) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(Error::new(
                ErrorKind::SignatureInvalid,
                format!("reading {}.sig: {err}", path.display()),
            )),
        }
    }

    /// Compile, check against the manifest, and instantiate.
    ///
    /// The single path from bytes to a running plugin: [`Host::load`],
    /// [`Host::load_binary`] and [`Plugin::reload`] all come through here, so
    /// there is no second route that could be built without the grant check.
    /// `extra_vars` are the per-load substitutions — `${plugin_dir}` today —
    /// layered on top of the builder's; a plugin keeps them so a reload
    /// resolves its manifest exactly as the original load did.
    ///
    /// This is also where the audit trail's load-time half is emitted. Every
    /// event here *reports* a verdict this function had already reached; none of
    /// them is reached in order to be reported. Nothing is computed at all
    /// unless a hook is installed, the component's digest included.
    pub(crate) fn instantiate_plugin(
        &self,
        name: &str,
        wasm: &[u8],
        extra_vars: BTreeMap<String, String>,
        kind: LoadKind,
        signature: Option<&[u8]>,
    ) -> Result<Plugin> {
        let audit = self.inner.audit.as_ref();
        // "From what bytes" is the question a plugin's name cannot answer, and
        // the one an incident opens with. Hashed only when someone is listening:
        // a pass over the component is not free, and a host with no hook must
        // not pay for a trail it will never read.
        let digest = audit.map_or_else(String::new, |_| sha256_hex(wasm));

        // Every failure path below goes through here, so a refusal cannot be
        // added without an event. `import` is `Some` only for the one refusal
        // that has a deciding import — which is the event someone reads after
        // an incident, so it carries the name rather than only the message.
        let refused = |import: Option<&ImportDecision>, err: Error| -> Error {
            if let Some(hook) = audit {
                let (import_name, requirement) = import.map_or((None, None), |decision| {
                    (Some(decision.import.as_str()), Some(decision.requirement))
                });
                hook.on_event(&match kind {
                    LoadKind::Load => AuditEvent::LoadRefused {
                        plugin: name,
                        sha256: &digest,
                        import: import_name,
                        requirement,
                        reason: err.message(),
                    },
                    LoadKind::Reload => AuditEvent::ReloadRefused {
                        plugin: name,
                        sha256: &digest,
                        import: import_name,
                        requirement,
                        reason: err.message(),
                    },
                });
            }
            err
        };

        // Before `compile`, and therefore before anything is cached: bytes that
        // do not verify must never become machine code, in memory or on disk.
        // It is also the cheap order — a hash and one curve operation instead
        // of a compilation.
        if let Err(err) = self.check_signature(wasm, signature) {
            return Err(refused(None, err));
        }

        let component = self.compile(wasm).map_err(|err| refused(None, err))?;

        let report = self.report_for(&component);
        // Before the verdict, so a refusal arrives already explained.
        if let Some(hook) = audit {
            for decision in &report.decisions {
                hook.on_event(&AuditEvent::ImportDecided {
                    plugin: name,
                    import: &decision.import,
                    requirement: decision.requirement,
                    verdict: Verdict::of(decision),
                });
            }
        }

        if !report.is_satisfied() {
            let denied: Vec<&str> = report.denied().map(|d| d.import.as_str()).collect();
            let err = Error::new(
                ErrorKind::PermissionDenied,
                format!(
                    "{name}: {} import(s) not granted by the manifest: {}\n{}",
                    denied.len(),
                    denied.join(", "),
                    report.describe()
                ),
            );
            return Err(refused(report.denied().next(), err));
        }

        let mut vars = self.inner.vars.clone();
        vars.extend(
            extra_vars
                .iter()
                .map(|(name, value)| (name.clone(), value.clone())),
        );

        let mut manifest = self.inner.manifest.clone();
        manifest
            .substitute(&vars)
            .map_err(|err| refused(None, err))?;

        let wiring = Wiring {
            manifest: &manifest,
            host_funcs: &self.inner.host_funcs,
            trace: self.inner.trace.as_ref(),
            audit,
            log_sink: self.inner.log_sink.as_ref(),
            profiling: self.inner.profiling,
        };
        let imports = report.decisions.len();
        let plugin = Plugin::instantiate(name, self, &component, &wiring, report, extra_vars)
            .map_err(|err| refused(None, err))?;

        // A reload has not happened yet: the replacement still has to take the
        // outgoing instance's state, and can still be sent back. `Plugin::reload`
        // says so once it has actually taken over.
        if let (Some(hook), LoadKind::Load) = (audit, kind) {
            hook.on_event(&AuditEvent::Loaded {
                plugin: name,
                sha256: &digest,
                imports,
            });
        }
        Ok(plugin)
    }

    /// Refuse a component the manifest's `[signature]` keys do not vouch for.
    ///
    /// A no-op when no keys are configured. That is the one place a watoots
    /// manifest does not default to deny, and ADR-0014 explains why: every
    /// existing embedder would stop loading plugins on upgrade. Once a key is
    /// listed the usual posture returns, including for a load that supplied no
    /// signature at all.
    fn check_signature(&self, wasm: &[u8], signature: Option<&[u8]>) -> Result<()> {
        if self.inner.trusted_keys.is_empty() {
            return Ok(());
        }
        let Some(signature) = signature else {
            return Err(Error::new(
                ErrorKind::SignatureInvalid,
                "this manifest lists signature.keys, so a plugin must be signed, and \
                 no signature was supplied. `Host::load` reads one from \
                 `<plugin>.wasm.sig`; loading from memory takes it as an argument",
            ));
        };
        signature::verify(wasm, signature, &self.inner.trusted_keys).map(|_| ())
    }

    /// Compile a component, going through the precompile cache when one is set.
    fn compile(&self, wasm: &[u8]) -> Result<Component> {
        let key = self.cache_key(wasm);

        // Before the file cache and before compiling: the same bytes are the
        // same machine code, and the only thing an extra instance actually
        // needs is a fresh `Store`.
        if let Ok(compiled) = self.inner.compiled.read()
            && let Some(component) = compiled.get(&key)
        {
            return Ok(component.clone());
        }

        let component = self.compile_uncached(wasm, &key)?;
        if let Ok(mut compiled) = self.inner.compiled.write() {
            compiled.insert(key, component.clone());
        }
        Ok(component)
    }

    /// How many components this host has built — compiled, or read back from
    /// the `.cwasm` cache.
    ///
    /// Loading the same component twice must not move this. It is the cheapest
    /// way to notice a missing `cache_dir`, or that something is handing you
    /// bytes that differ when you believed they did not.
    #[must_use]
    pub fn compiles(&self) -> u64 {
        self.inner.compiles.load(Ordering::Relaxed)
    }

    fn compile_uncached(&self, wasm: &[u8], key: &str) -> Result<Component> {
        self.inner.compiles.fetch_add(1, Ordering::Relaxed);
        let Some(dir) = &self.inner.cache_dir else {
            return self.compile_fresh(wasm);
        };

        let path = dir.join(format!("{key}.cwasm"));

        if path.is_file() {
            // SAFETY: the cache key includes the engine's own compatibility
            // hash, so a file under this name was produced by an engine that
            // can load it. The cache directory has to be trusted: anyone who
            // can write here can hand us machine code to run, which is why the
            // directory is opt-in rather than defaulted somewhere shared.
            match unsafe { Component::deserialize_file(&self.inner.engine, &path) } {
                Ok(component) => return Ok(component),
                // A truncated or half-written file should cost a recompile, not
                // a hard failure.
                Err(_) => {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }

        let component = self.compile_fresh(wasm)?;

        if let Ok(serialized) = self.inner.engine.precompile_component(wasm) {
            let _ = std::fs::create_dir_all(dir);
            // Write-then-rename so a concurrent reader never sees a partial
            // file, and a crash mid-write leaves no poisoned entry.
            let temp = path.with_extension(format!("cwasm.tmp{}", std::process::id()));
            if std::fs::write(&temp, &serialized).is_ok() && std::fs::rename(&temp, &path).is_err()
            {
                let _ = std::fs::remove_file(&temp);
            }
        }

        Ok(component)
    }

    fn compile_fresh(&self, wasm: &[u8]) -> Result<Component> {
        Component::new(&self.inner.engine, wasm)
            .map_err(|err| Error::new(ErrorKind::Load, format!("{err:?}")))
    }

    /// Cache key: this engine's configuration plus the exact component bytes.
    ///
    /// The engine half is what makes reusing a `.cwasm` safe — a cache written
    /// by a differently configured engine simply does not collide.
    fn cache_key(&self, wasm: &[u8]) -> String {
        let mut hasher = Sha256Hasher::default();
        self.inner
            .engine
            .precompile_compatibility_hash()
            .hash(&mut hasher);
        hasher.0.update(wasm);
        hex(&hasher.0.finalize())
    }

    fn report_for(&self, component: &Component) -> GrantReport {
        let engine = &self.inner.engine;
        let component_type = component.component_type();
        let declared: Vec<imports::ComponentImport<'_>> = component_type
            .imports(engine)
            .map(|(name, extern_)| imports::ComponentImport {
                name,
                has_functions: has_callable_functions(&extern_.ty, engine),
            })
            .collect();
        imports::check(
            declared,
            &self.inner.manifest.permissions,
            &self.inner.host_provided,
        )
    }
}

impl fmt::Debug for Host {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The engine and the ticker are not worth printing; the policy is.
        f.debug_struct("Host")
            .field("manifest", &self.inner.manifest)
            .field("host_provided", &self.inner.host_provided)
            .field(
                "host_funcs",
                &self.inner.host_funcs.keys().collect::<Vec<_>>(),
            )
            .field("vars", &self.inner.vars)
            .field("cache_dir", &self.inner.cache_dir)
            .field("tracing", &self.inner.trace.is_some())
            .field("auditing", &self.inner.audit.is_some())
            .field("log_sink", &self.inner.log_sink.is_some())
            .field("profiling", &self.inner.profiling)
            .finish_non_exhaustive()
    }
}

/// One function a component expects a host to provide.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ImportedFunction {
    /// Interface name, version included.
    pub interface: String,
    /// Function name within the interface.
    pub func: String,
}

/// Builder for [`Host`].
#[derive(Default)]
pub struct HostBuilder {
    manifest: Manifest,
    host_provided: BTreeSet<String>,
    host_funcs: BTreeMap<String, BTreeMap<String, HostFunc>>,
    vars: BTreeMap<String, String>,
    cache_dir: Option<PathBuf>,
    trace: Option<Arc<dyn TraceHook>>,
    audit: Option<Arc<dyn AuditHook>>,
    log_sink: Option<LogSink>,
    profiling: Option<Profiling>,
}

impl fmt::Debug for HostBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostBuilder")
            .field("manifest", &self.manifest)
            .field("host_provided", &self.host_provided)
            .finish_non_exhaustive()
    }
}

impl HostBuilder {
    /// Use this manifest.
    #[must_use]
    pub fn manifest(mut self, manifest: Manifest) -> Self {
        self.manifest = manifest;
        self
    }

    /// Read the manifest from a TOML file.
    pub fn manifest_from_file(mut self, path: impl AsRef<Path>) -> Result<Self> {
        self.manifest = Manifest::from_file(path)?;
        Ok(self)
    }

    /// Declare that the application serves this interface, without supplying
    /// its functions yet.
    ///
    /// [`HostBuilder::host_func`] does this for you. Use this directly only to
    /// let [`Host::inspect`] answer honestly for a world you have not finished
    /// implementing — a component importing it will still fail to instantiate.
    #[must_use]
    pub fn provide_interface(mut self, interface: impl Into<String>) -> Self {
        self.host_provided.insert(unversioned(&interface.into()));
        self
    }

    /// Serve one function of one interface to plugins.
    ///
    /// `interface` must be spelled the way the component imports it, version
    /// included — `watoots:example/log@0.1.0`. That is what the linker matches
    /// on, and wasmtime will resolve a semver-compatible drift from there. The
    /// grant check compares unversioned names, so a manifest never has to be
    /// re-stated when a guest is rebuilt against a new patch of the interface.
    ///
    /// Registering a function also declares its interface, so the grant check
    /// and what the linker actually provides cannot drift apart.
    #[must_use]
    pub fn host_func<F>(mut self, interface: &str, func: &str, implementation: F) -> Self
    where
        F: Fn(&HostCall<'_>) -> Result<Vec<Val>> + Send + Sync + 'static,
    {
        self.host_provided.insert(unversioned(interface));
        self.host_funcs
            .entry(interface.to_string())
            .or_default()
            .insert(func.to_string(), Arc::new(implementation));
        self
    }

    /// Define a `${name}` substitution for manifest paths.
    #[must_use]
    pub fn var(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.vars.insert(name.into(), value.into());
        self
    }

    /// Cache compiled components as `.cwasm` files under this directory.
    ///
    /// This is the cache that survives the process. A [`Host`] already avoids
    /// rebuilding a component it has compiled before, so repeat loads within
    /// one run are cheap either way; what a directory buys is the *first* load
    /// of the next run, and that is where the cost is. A `wasm32-wasip2` Rust
    /// guest compiles in milliseconds, but an interpreter guest is a different
    /// order of magnitude — the sample Python plugin is 18 MB of CPython and
    /// takes seconds. A long-lived server will not notice; a command-line tool
    /// pays it on every invocation.
    ///
    /// The directory must be trusted: entries are machine code that the engine
    /// loads without re-validating, so write access to it is equivalent to code
    /// execution in the host process. There is deliberately no default —
    /// picking a shared location on the user's behalf is picking who may
    /// execute code in their process.
    ///
    /// A corrupt or truncated entry costs a recompile rather than an error, and
    /// the key includes the engine's compatibility hash, so a Wasmtime upgrade
    /// invalidates entries rather than loading incompatible machine code.
    #[must_use]
    pub fn cache_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cache_dir = Some(dir.into());
        self
    }

    /// Observe every crossing of the host/plugin boundary.
    #[must_use]
    pub fn trace_hook(mut self, hook: Arc<dyn TraceHook>) -> Self {
        self.trace = Some(hook);
        self
    }

    /// Observe every authorisation decision: what a plugin was permitted to do,
    /// and what it was refused.
    ///
    /// The sibling of [`HostBuilder::trace_hook`] and deliberately not the same
    /// hook. A trace answers *what happened* and carries the argument values to
    /// prove it; this answers *what was allowed* and carries names and verdicts
    /// only, which is what makes an audit line safe to keep and to paste into an
    /// issue. See [`AuditEvent`](crate::AuditEvent) and ADR-0011.
    ///
    /// **Off unless you call this.** An application with no hook installed gets
    /// no audit trail — not a default one written somewhere. A library that
    /// wrote to stderr uninvited would be badly behaved, and your logging system
    /// is better than the one we would ship.
    ///
    /// Setting a hook twice replaces the first; there is one, and watoots does
    /// no fan-out.
    #[must_use]
    pub fn audit_hook(mut self, hook: Arc<dyn AuditHook>) -> Self {
        self.audit = Some(hook);
        self
    }

    /// Receive the plugin's `wasi:logging` messages.
    ///
    /// Whether the interface links at all is the manifest's decision, not this
    /// one: `permissions.logging` grants it and sets the level ceiling, and a
    /// component importing `wasi:logging` under a manifest that says nothing
    /// fails to load however many sinks are registered. A grant with no sink is
    /// legal and discards — the application chose not to listen — but it is
    /// almost always a bug, so register one.
    ///
    /// Setting a sink twice replaces the first; there is one, and watoots does
    /// no fan-out.
    #[must_use]
    pub fn log_sink<F>(mut self, sink: F) -> Self
    where
        F: Fn(&LogRecord<'_>) + Send + Sync + 'static,
    {
        self.log_sink = Some(Arc::new(sink));
        self
    }

    /// Split every plugin's time into guest, host-call and marshalling.
    ///
    /// Opt-in, because it is not free: a call hook fires on every host↔guest
    /// transition. It answers the question `PluginStats` cannot — *where* the
    /// time went, per WIT function — and like `PluginStats` it observes at the
    /// boundary, so a guest can neither forge nor inflate it. Read the result
    /// with [`Plugin::profile`].
    ///
    /// Refused alongside [`HostBuilder::trace_hook`]: profiling changes timing,
    /// and a trace recorded under it would record a run nobody can reproduce.
    /// See ADR-0009.
    #[must_use]
    pub fn profile(mut self) -> Self {
        self.profiling.get_or_insert_default();
        self
    }

    /// Also sample guest stacks, for a Firefox Profiler JSON.
    ///
    /// Implies [`HostBuilder::profile`], and adds the half of the answer that
    /// is Wasmtime's: which *guest* function is hot, which the boundary buckets
    /// cannot say. Write the result with [`Plugin::write_guest_profile`].
    ///
    /// Sampling is driven from the epoch deadline, which also enforces
    /// `limits.timeout`. They share it and **the timeout wins**: the callback
    /// samples and then continues with `min(interval, remaining timeout)`, so
    /// profiling a runaway plugin does not keep it alive. The interval is
    /// rounded up to a whole epoch tick (1ms).
    #[must_use]
    pub fn profile_guest_samples(mut self, interval: Duration) -> Self {
        self.profiling = Some(Profiling {
            sample_interval: Some(interval),
        });
        self
    }

    /// Build the host and its engine.
    pub fn build(self) -> Result<Host> {
        let limits = &self.manifest.limits;

        // Timing is exactly what the determinism knobs exist to pin, and a
        // profiler perturbs it. Refused rather than silently permitted, because
        // the cost lands on whoever tries to replay the trace afterwards.
        if self.profiling.is_some() && self.trace.is_some() {
            return Err(Error::new(
                ErrorKind::InvalidArgument,
                "profiling and a trace hook cannot both be enabled: profiling \
                 changes timing, so a session recorded under it would not \
                 reproduce. Record first, then profile the replay.",
            ));
        }

        // Sampling is driven from the epoch deadline too, so it needs the
        // interruption machinery and the ticker even when no timeout asked for
        // them.
        let samples = self
            .profiling
            .is_some_and(|options| options.sample_interval.is_some());
        let needs_epoch = limits.timeout.is_some() || samples;

        let mut config = Config::new();
        config.wasm_component_model(true);
        config.consume_fuel(limits.fuel.is_some());
        config.epoch_interruption(needs_epoch);

        // Two runs of the same guest must agree on float bit patterns, or a
        // recorded trace stops reproducing on a different machine.
        if self.manifest.determinism.enabled {
            config.cranelift_nan_canonicalization(true);
            config.relaxed_simd_deterministic(true);
        }

        let engine = Engine::new(&config)
            .map_err(|err| Error::new(ErrorKind::Internal, format!("engine config: {err:?}")))?;

        let ticker = needs_epoch.then(|| EpochTicker::spawn(engine.clone()));

        Ok(Host {
            inner: Arc::new(HostInner {
                compiled: RwLock::new(CompiledCache::default()),
                compiles: AtomicU64::new(0),
                engine,
                trusted_keys: signature::parse_keys(&self.manifest.signature.keys)?,
                manifest: self.manifest,
                host_provided: self.host_provided,
                host_funcs: self.host_funcs,
                vars: self.vars,
                cache_dir: self.cache_dir,
                trace: self.trace,
                audit: self.audit,
                log_sink: self.log_sink,
                profiling: self.profiling,
                _ticker: ticker,
            }),
        })
    }
}

/// Read a component from disk, saying which file was missing.
pub(crate) fn read_component(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path).map_err(|err| {
        Error::new(
            ErrorKind::NotFound,
            format!("cannot read component {}: {err}", path.display()),
        )
    })
}

/// The `${plugin_dir}` substitution a file-backed load defines.
///
/// Shared with [`Plugin::reload_from_file`], which re-points it at wherever the
/// replacement came from: a manifest that grants `${plugin_dir}/cache` means
/// the new component's directory, not the old one's.
pub(crate) fn plugin_dir_var(path: &Path) -> BTreeMap<String, String> {
    let dir = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .display()
        .to_string();
    BTreeMap::from([("plugin_dir".to_string(), dir)])
}

/// Strip an `@version` suffix, which is how grants are matched.
fn unversioned(interface: &str) -> String {
    imports::InterfaceRef::parse(interface)
        .map_or_else(|| interface.to_string(), |iface| iface.unversioned())
}

/// Whether an imported item exposes anything the guest could actually call.
///
/// An instance holding only type definitions is not a capability, however
/// alarming its name looks in a grant list.
fn has_callable_functions(item: &ComponentItem, engine: &Engine) -> bool {
    match item {
        ComponentItem::ComponentFunc(_) | ComponentItem::CoreFunc(_) => true,
        ComponentItem::ComponentInstance(instance) => instance
            .exports(engine)
            .any(|(_, nested)| has_callable_functions(&nested.ty, engine)),
        // A module or a nested component import is opaque to us; treat it as
        // callable so it cannot slip through as "just types".
        ComponentItem::Module(_) | ComponentItem::Component(_) => true,
        ComponentItem::Type(_) | ComponentItem::Resource(_) => false,
    }
}

/// Feeds `std::hash::Hash` output into SHA-256.
///
/// Wasmtime hands out its compatibility fingerprint as an opaque `impl Hash`,
/// and `DefaultHasher` is explicitly not stable across releases — no use for
/// something that names files on disk.
#[derive(Default)]
struct Sha256Hasher(Sha256);

impl Hasher for Sha256Hasher {
    fn write(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }

    fn finish(&self) -> u64 {
        let digest = self.0.clone().finalize();
        u64::from_le_bytes(digest[..8].try_into().expect("sha256 is 32 bytes"))
    }
}

/// SHA-256 of a component, lowercase hex.
///
/// The same digest `watoots_trace::Trace::hash_component` writes into a trace
/// header, so an audit line and a recording name the same bytes the same way.
/// Only ever called when an audit hook is installed.
pub(crate) fn sha256_hex(wasm: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(wasm);
    hex(&hasher.finalize())
}

fn hex(bytes: &[u8]) -> String {
    use fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

/// Advances the engine epoch on a fixed tick so per-call deadlines can fire.
struct EpochTicker {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl EpochTicker {
    fn spawn(engine: Engine) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let handle = std::thread::Builder::new()
            .name("watoots-epoch".to_string())
            .spawn(move || {
                while !flag.load(Ordering::Relaxed) {
                    std::thread::sleep(EPOCH_TICK);
                    engine.increment_epoch();
                }
            })
            .expect("spawning the epoch ticker");

        Self {
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for EpochTicker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}
