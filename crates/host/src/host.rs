//! The engine, the policy, and loading plugins under it.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use sha2::{Digest, Sha256};
use wasmtime::component::types::ComponentItem;
use wasmtime::component::{Component, Type, Val};
use wasmtime::{Config, Engine};

use crate::imports::{self, GrantReport};
use crate::manifest::Manifest;
use crate::plugin::{Plugin, Wiring};
use crate::trace::TraceHook;
use crate::{Error, ErrorKind, Result};

/// How often the epoch ticker advances the engine's epoch.
///
/// Deadlines are expressed in whole ticks, so this is also the granularity of a
/// `timeout`: a 200ms timeout is 200 ticks.
const EPOCH_TICK: Duration = Duration::from_millis(1);

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
#[derive(Clone)]
pub struct Host {
    inner: Arc<HostInner>,
}

struct HostInner {
    engine: Engine,
    manifest: Manifest,
    host_provided: BTreeSet<String>,
    host_funcs: BTreeMap<String, BTreeMap<String, HostFunc>>,
    vars: BTreeMap<String, String>,
    cache_dir: Option<PathBuf>,
    trace: Option<Arc<dyn TraceHook>>,
    /// Kept alive for as long as the host is; dropping it stops the thread.
    _ticker: Option<EpochTicker>,
}

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

    /// Load a component from disk.
    ///
    /// `${plugin_dir}` expands to the directory the component was loaded from,
    /// on top of any variables set on the builder.
    pub fn load(&self, path: impl AsRef<Path>) -> Result<Plugin> {
        let path = path.as_ref();
        let wasm = std::fs::read(path).map_err(|err| {
            Error::new(
                ErrorKind::NotFound,
                format!("cannot read component {}: {err}", path.display()),
            )
        })?;

        let plugin_dir = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
            .display()
            .to_string();

        let name = path.file_stem().map_or_else(
            || path.display().to_string(),
            |s| s.to_string_lossy().into(),
        );

        self.load_binary_with(&name, &wasm, |vars| {
            vars.insert("plugin_dir".to_string(), plugin_dir);
        })
    }

    /// Load a component already in memory.
    ///
    /// `${plugin_dir}` is not defined for this path — there is no directory to
    /// point it at — so a manifest using it must be loaded with [`Host::load`].
    pub fn load_binary(&self, name: &str, wasm: &[u8]) -> Result<Plugin> {
        self.load_binary_with(name, wasm, |_| {})
    }

    fn load_binary_with(
        &self,
        name: &str,
        wasm: &[u8],
        extra_vars: impl FnOnce(&mut BTreeMap<String, String>),
    ) -> Result<Plugin> {
        let component = self.compile(wasm)?;

        let report = self.report_for(&component);
        if !report.is_satisfied() {
            let denied: Vec<&str> = report.denied().map(|d| d.import.as_str()).collect();
            return Err(Error::new(
                ErrorKind::PermissionDenied,
                format!(
                    "{name}: {} import(s) not granted by the manifest: {}\n{}",
                    denied.len(),
                    denied.join(", "),
                    report.describe()
                ),
            ));
        }

        let mut vars = self.inner.vars.clone();
        extra_vars(&mut vars);

        let mut manifest = self.inner.manifest.clone();
        manifest.substitute(&vars)?;

        let wiring = Wiring {
            manifest: &manifest,
            host_funcs: &self.inner.host_funcs,
            trace: self.inner.trace.as_ref(),
        };
        Plugin::instantiate(name, &self.inner.engine, &component, &wiring, report)
    }

    /// Compile a component, going through the precompile cache when one is set.
    fn compile(&self, wasm: &[u8]) -> Result<Component> {
        let Some(dir) = &self.inner.cache_dir else {
            return self.compile_fresh(wasm);
        };

        let path = dir.join(format!("{}.cwasm", self.cache_key(wasm)));

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
            .finish_non_exhaustive()
    }
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
    /// The directory must be trusted: entries are machine code that the engine
    /// loads without re-validating, so write access to it is equivalent to code
    /// execution in the host process. There is deliberately no default.
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

    /// Build the host and its engine.
    pub fn build(self) -> Result<Host> {
        let limits = &self.manifest.limits;

        // An empty `net` list is fine: it grants the socket interfaces while
        // wasmtime-wasi's own default refuses every connection. A non-empty
        // allowlist is not, because nothing enforces it yet, and the failure
        // mode of pretending otherwise is handing a plugin the whole network
        // because the manifest named one host.
        if self
            .manifest
            .permissions
            .net
            .as_ref()
            .is_some_and(|hosts| !hosts.is_empty())
        {
            return Err(Error::new(
                ErrorKind::Manifest,
                "a non-empty permissions.net allowlist is not enforced yet; \
                 use `net = []` to grant the interfaces with no reachable hosts",
            ));
        }

        let mut config = Config::new();
        config.wasm_component_model(true);
        config.consume_fuel(limits.fuel.is_some());
        config.epoch_interruption(limits.timeout.is_some());

        let engine = Engine::new(&config)
            .map_err(|err| Error::new(ErrorKind::Internal, format!("engine config: {err:?}")))?;

        let ticker = limits
            .timeout
            .is_some()
            .then(|| EpochTicker::spawn(engine.clone()));

        Ok(Host {
            inner: Arc::new(HostInner {
                engine,
                manifest: self.manifest,
                host_provided: self.host_provided,
                host_funcs: self.host_funcs,
                vars: self.vars,
                cache_dir: self.cache_dir,
                trace: self.trace,
                _ticker: ticker,
            }),
        })
    }
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
