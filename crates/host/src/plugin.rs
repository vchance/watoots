//! A loaded, instantiated component and the store it runs in.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use wasmtime::component::{Component, Linker, ResourceTable, Type, Val};
use wasmtime::{Engine, GuestProfiler, Store, StoreLimits, StoreLimitsBuilder, UpdateDeadline};
use wasmtime_wasi::{FsPerms, WasiCtx, WasiCtxView, WasiView};

use crate::audit::{AuditEvent, AuditHook, Ceiling};
use crate::host::{
    HostFunc, LoadKind, LogRecord, LogSink, epoch_ticks, plugin_dir_var, read_component, sha256_hex,
};
use crate::imports::GrantReport;
use crate::manifest::{Limits, LogLevel, Manifest};
use crate::profile::{Deadline, PluginProfile, ProfileState, Profiling};
use crate::trace::{Outcome, TraceEvent, TraceHook};
use crate::{Error, ErrorKind, Host, Result};

/// The export a plugin uses to hand its state out before it is replaced.
///
/// Optional: a component that does not export it reloads with no state, which
/// ADR-0010 calls the honest behaviour rather than an error. It takes no
/// parameters and returns exactly one value, of whatever type the world
/// declares — the host carries the value without interpreting it.
pub const SAVE_STATE_EXPORT: &str = "save-state";

/// The export a plugin uses to take state back after it replaces another.
///
/// The mirror of [`SAVE_STATE_EXPORT`]: one parameter, no results, and the
/// parameter's type must be the one `save-state` returned. Also optional.
pub const RESTORE_STATE_EXPORT: &str = "restore-state";

/// The `wasi:logging` interface, spelled as the proposal publishes it.
///
/// Verified against <https://github.com/WebAssembly/wasi-logging> at commit
/// `d31c41d0d9eed81aabe02333d0025d42acf3fb75` (2024-10-02, the newest change to
/// `wit/` as of 2026-09-03). `wit/world.wit` declares
/// `package wasi:logging@0.1.0-draft;` and `wit/logging.wit` declares
/// `interface logging` with `enum level { trace, debug, info, warn, error,
/// critical }` and `log: func(level: level, context: string, message: string);`
/// — no result. It is a Phase 1 proposal (ADR-0006 accepts that risk), and the
/// `-draft` suffix is part of the version: semver treats a prerelease as
/// compatible with nothing but itself, so a guest built against a later
/// `0.1.0` would *not* resolve to this name.
const LOGGING_VERSIONED: &str = "wasi:logging/logging@0.1.0-draft";

/// The same interface with the version stripped.
///
/// Registered alongside the versioned spelling because a guest built from a
/// vendored copy of the WIT that dropped the package version imports the bare
/// name, and the linker matches literally. Costs one empty instance in the
/// linker and saves an unresolvable-import failure that reads like a bug in the
/// component.
const LOGGING_UNVERSIONED: &str = "wasi:logging/logging";

/// The function `wasi:logging/logging` declares.
const LOGGING_FUNC: &str = "log";

/// Everything the guest can reach, plus the ceilings it runs under.
struct State {
    wasi: WasiCtx,
    table: ResourceTable,
    limits: MeteredLimits,
    log: LogBudget,
    counters: Counters,
    /// The per-call epoch budget. Present whether or not profiling is on: the
    /// deadline enforces `limits.timeout` first and samples second.
    deadline: Deadline,
    /// `None` unless the host asked for profiling, which is what makes the
    /// feature free when it is off — there is no call hook either.
    profile: Option<ProfileState>,
}

/// `StoreLimits`, plus the high-water mark it passes through.
///
/// Wasmtime's `StoreLimits` enforces the ceiling and does not report what was
/// actually used, so peak memory needs a limiter of our own rather than an
/// accessor. This is the one number an operator asks for first — "which plugin
/// is eating my memory" — and the guest cannot lie about it, because it is
/// observed at the growth request rather than reported by the plugin.
struct MeteredLimits {
    inner: StoreLimits,
    peak_memory: usize,
    /// The audit hook and the name to report under, or `None` when nobody is
    /// listening — which is also why the name is not stored unconditionally.
    audit: Option<AuditFor>,
}

/// An audit hook plus the plugin name to report it under.
///
/// The hook is handed a `&str`, and the places that emit inside a store — the
/// limiter, the logging shim — do not otherwise have the plugin's name to hand.
struct AuditFor {
    hook: Arc<dyn AuditHook>,
    plugin: String,
}

impl wasmtime::ResourceLimiter for MeteredLimits {
    fn memory_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        let allowed = self.inner.memory_growing(current, desired, maximum)?;
        if allowed {
            self.peak_memory = self.peak_memory.max(desired);
        } else if let Some(audit) = &self.audit {
            // The only ceiling a plugin never sees as an error: a refused
            // growth makes `memory.grow` return -1, so without this the fact
            // that a plugin hit `limits.memory` reaches nobody at all. The
            // refusal itself is the limiter's, unchanged.
            audit.hook.on_event(&AuditEvent::CeilingSpent {
                plugin: &audit.plugin,
                limit: Ceiling::Memory,
            });
        }
        Ok(allowed)
    }

    fn table_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        self.inner.table_growing(current, desired, maximum)
    }
}

/// Totals across a plugin's lifetime, accumulated as each call ends.
///
/// The per-call budgets in `LogBudget` are re-armed by [`arm`]; these are not.
#[derive(Debug, Clone, Copy, Default)]
struct Counters {
    calls: u64,
    fuel_consumed: u64,
    log_messages: u64,
    log_bytes: u64,
    /// Carried across a reload and incremented by it; see
    /// [`PluginStats::reloads`].
    reloads: u64,
}

/// What a host has observed about one plugin.
///
/// The half of "metrics" worth having, and the reason ADR-0006 declines to let
/// a plugin report its own: these numbers are observed at the boundary, so a
/// guest cannot inflate or forge them, and their cardinality is fixed by this
/// struct rather than by untrusted input.
///
/// # Across a reload
///
/// Every counter here **accumulates** across [`Plugin::reload`]; only
/// [`reloads`](PluginStats::reloads) changes. These numbers answer "what has
/// this plugin cost me", and a reload is a new build of the same plugin rather
/// than a new plugin — a host that reloads on every file change would otherwise
/// report near-zero fuel forever. [`Plugin::profile`] takes the opposite
/// decision, for the opposite reason; see its documentation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PluginStats {
    /// Calls that have completed, successfully or not.
    pub calls: u64,
    /// Fuel burned across those calls. Zero when the manifest sets no fuel
    /// limit, because nothing is metered then.
    pub fuel_consumed: u64,
    /// The largest linear-memory size the guest was granted, in bytes.
    pub peak_memory_bytes: u64,
    /// `wasi:logging` messages emitted, and their total size.
    pub log_messages: u64,
    /// Bytes of log message emitted.
    pub log_bytes: u64,
    /// Imports the component declares.
    ///
    /// The *current* bytes': a reload re-runs the intersection check, so this
    /// and `imports_denied` describe the component running now rather than the
    /// one first loaded.
    pub imports_declared: usize,
    /// How many of those the manifest did not grant. Non-zero only where the
    /// application served them itself, since otherwise the load would have
    /// failed.
    pub imports_denied: usize,
    /// Successful [`Plugin::reload`]s. Zero for a plugin still running the
    /// bytes it was loaded with; a failed reload does not count, because
    /// nothing was replaced.
    pub reloads: u64,
}

/// What one [`Plugin::reload`] did with the outgoing instance's state.
///
/// The reload itself either happened or returned an error — this says what
/// crossed, which is the part a host may want to log or refuse. `state_saved`
/// without `state_restored` is the case worth noticing: the outgoing build had
/// state to hand over and the incoming one declared nowhere to put it, so it
/// was dropped. That is not an error (ADR-0010 makes the hooks optional in both
/// directions, so that a rollback to a build without them still works), but it
/// is worth a line in a log.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReloadReport {
    /// The outgoing instance exported [`SAVE_STATE_EXPORT`] and it returned.
    pub state_saved: bool,
    /// The incoming instance exported [`RESTORE_STATE_EXPORT`] and was given
    /// the value.
    pub state_restored: bool,
    /// Reloads this plugin has survived, this one included.
    pub reloads: u64,
}

impl ReloadReport {
    /// Whether state was handed over and had nowhere to land.
    #[must_use]
    pub fn state_dropped(&self) -> bool {
        self.state_saved && !self.state_restored
    }
}

/// How much more a plugin may say during the call in progress.
///
/// Per call, like fuel and the deadline, and re-armed by [`arm`]. Per plugin
/// would make a long-lived plugin's first busy call spend a budget its
/// thousandth needs, which is the same reasoning that makes fuel per-call.
#[derive(Debug, Clone, Copy)]
struct LogBudget {
    bytes_allowed: u64,
    messages_allowed: u64,
    bytes_used: u64,
    messages_used: u64,
}

impl LogBudget {
    /// Messages and bytes spent during the call in progress.
    fn used(&self) -> (u64, u64) {
        (self.messages_used, self.bytes_used)
    }

    fn new(limits: &Limits) -> Self {
        Self {
            bytes_allowed: limits.log_bytes,
            messages_allowed: limits.log_messages,
            bytes_used: 0,
            messages_used: 0,
        }
    }

    fn rearm(&mut self) {
        self.bytes_used = 0;
        self.messages_used = 0;
    }

    /// Charge one message, or say which ceiling it does not fit under.
    ///
    /// Charged before the level ceiling filters, deliberately: what this bounds
    /// is the work the *host* does lifting a string out of guest memory, and
    /// that has already happened by the time we can read the level. Charging
    /// only what survives the filter would let `logging = "critical"` be a
    /// licence to push unbounded bytes across the boundary at `trace`.
    ///
    /// The failure names the [`Ceiling`] as well as explaining itself, because
    /// the audit trail reports which limit was spent and the message is the
    /// wrong thing to recover that from.
    fn charge(&mut self, bytes: u64) -> std::result::Result<(), (Ceiling, String)> {
        self.messages_used = self.messages_used.saturating_add(1);
        self.bytes_used = self.bytes_used.saturating_add(bytes);

        if self.messages_used > self.messages_allowed {
            return Err((
                Ceiling::LogMessages,
                format!(
                    "log message limit exceeded: {} message(s) in one call, \
                     limits.log_messages is {}",
                    self.messages_used, self.messages_allowed
                ),
            ));
        }
        if self.bytes_used > self.bytes_allowed {
            return Err((
                Ceiling::LogBytes,
                format!(
                    "log volume limit exceeded: {} byte(s) in one call, limits.log_bytes is {}",
                    self.bytes_used, self.bytes_allowed
                ),
            ));
        }
        Ok(())
    }
}

/// Marker carried by the wasmtime error a blown log budget produces.
///
/// A ceiling is not a misbehaving guest, and the two are reported differently:
/// [`Plugin::classify_call_error`] downcasts to this so a log overrun lands as
/// [`ErrorKind::LimitExceeded`], next to out-of-fuel and the deadline, rather
/// than as a trap.
#[derive(Debug)]
struct LogVolumeExceeded(String);

impl fmt::Display for LogVolumeExceeded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for LogVolumeExceeded {}

impl WasiView for State {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

/// What a plugin needs at instantiation time beyond its bytes.
pub(crate) struct Wiring<'a> {
    pub manifest: &'a Manifest,
    pub host_funcs: &'a BTreeMap<String, BTreeMap<String, HostFunc>>,
    pub trace: Option<&'a Arc<dyn TraceHook>>,
    pub audit: Option<&'a Arc<dyn AuditHook>>,
    pub log_sink: Option<&'a LogSink>,
    pub profiling: Option<Profiling>,
}

/// One instantiated plugin.
///
/// A plugin owns its store, so limits are per-plugin: one plugin exhausting its
/// fuel or memory says nothing about its neighbours.
pub struct Plugin {
    name: String,
    store: Store<State>,
    instance: wasmtime::component::Instance,
    limits: Limits,
    report: GrantReport,
    trace: Option<Arc<dyn TraceHook>>,
    /// Mirrors the host's. Kept here so the two decisions reached *after* a
    /// plugin exists — a ceiling spent during a call, a reload refused — can be
    /// reported from where they are made.
    audit: Option<Arc<dyn AuditHook>>,
    /// Mirrors `State::profile.is_some()`, so the hot path in [`Plugin::call`]
    /// costs one bool rather than a store borrow.
    profiling: bool,
    /// The host this was loaded under, kept so [`Plugin::reload`] can go back
    /// through the same load path — same engine, same manifest, same check.
    /// Cloning a `Host` shares its engine, so this costs one `Arc`.
    ///
    /// It also fixes something that was quietly wrong: a `Host` owns the epoch
    /// ticker, and dropping the last one stops the thread. A plugin that
    /// outlived its host used to keep running with the epoch frozen, which
    /// means `limits.timeout` silently stopped firing — on a plugin already
    /// running. Holding the host here makes that impossible.
    host: Host,
    /// The per-load `${...}` substitutions this plugin's manifest was resolved
    /// with. Kept so a reload resolves it the same way rather than quietly
    /// changing what the manifest granted.
    vars: BTreeMap<String, String>,
}

impl fmt::Debug for Plugin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Plugin")
            .field("name", &self.name)
            .field("limits", &self.limits)
            .field("imports", &self.report.decisions.len())
            .field("reloads", &self.store.data().counters.reloads)
            .finish_non_exhaustive()
    }
}

impl Plugin {
    pub(crate) fn instantiate(
        name: &str,
        host: &Host,
        component: &Component,
        wiring: &Wiring<'_>,
        report: GrantReport,
        vars: BTreeMap<String, String>,
    ) -> Result<Self> {
        let engine = host.engine();
        let manifest = wiring.manifest;
        let wasi = build_wasi_ctx(manifest)?;

        let memory = usize::try_from(manifest.limits.memory).map_err(|_| {
            Error::new(
                ErrorKind::InvalidArgument,
                format!(
                    "limits.memory of {} bytes does not fit in this host's address space",
                    manifest.limits.memory
                ),
            )
        })?;

        // The sampler is built before the store because it needs the component
        // it will be reporting stacks from, and because failing to build it is
        // a load failure rather than something to discover at the first sample.
        let profile = match wiring.profiling {
            None => None,
            Some(options) => Some(ProfileState::new(build_sampler(
                name, engine, component, options,
            )?)),
        };

        let deadline = Deadline::new(
            manifest.limits.timeout.map(epoch_ticks),
            wiring
                .profiling
                .and_then(|options| options.sample_interval)
                .map(epoch_ticks),
        );

        let state = State {
            wasi,
            table: ResourceTable::new(),
            limits: MeteredLimits {
                inner: StoreLimitsBuilder::new().memory_size(memory).build(),
                peak_memory: 0,
                audit: wiring.audit.map(|hook| AuditFor {
                    hook: Arc::clone(hook),
                    plugin: name.to_string(),
                }),
            },
            log: LogBudget::new(&manifest.limits),
            counters: Counters::default(),
            deadline,
            profile,
        };

        let profiling = state.profile.is_some();
        let mut store = Store::new(engine, state);
        store.limiter(|state| &mut state.limits);
        if profiling {
            install_profiler(&mut store);
        }
        if deadline.samples() {
            install_epoch_callback(&mut store);
        }

        let mut linker: Linker<State> = Linker::new(engine);
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker)
            .map_err(|err| Error::new(ErrorKind::Internal, format!("wiring WASI: {err:?}")))?;

        install_logging(&mut linker, name, wiring)?;
        install_host_funcs(&mut linker, name, wiring)?;

        // A fresh store starts with no fuel and a deadline of zero, so the
        // budgets have to be armed before instantiation runs any guest code,
        // not just before the first call.
        arm(&mut store, &manifest.limits, name)?;

        let instance = linker.instantiate(&mut store, component).map_err(|err| {
            Error::new(
                ErrorKind::Load,
                format!("{name}: instantiation failed: {err:?}"),
            )
        })?;

        // Instantiation ran guest code with the call hook already installed.
        // That time belongs to no call, so drop it rather than let the first
        // `call` inherit it.
        if let Some(profile) = store.data_mut().profile.as_mut() {
            profile.reset_call();
        }

        Ok(Self {
            name: name.to_string(),
            store,
            instance,
            limits: manifest.limits.clone(),
            report,
            trace: wiring.trace.map(Arc::clone),
            audit: wiring.audit.map(Arc::clone),
            profiling,
            host: host.clone(),
            vars,
        })
    }

    /// The name this plugin was loaded under.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// What this plugin's imports were granted at load time.
    #[must_use]
    pub fn grants(&self) -> &GrantReport {
        &self.report
    }

    /// What the host has observed about this plugin since it was loaded.
    ///
    /// Observed at the boundary rather than reported by the guest, so a plugin
    /// can neither forge nor inflate these, and the set of numbers is fixed
    /// here rather than by anything the plugin sends. That is the distinction
    /// ADR-0006 draws when it declines to build guest-emitted metrics.
    #[must_use]
    pub fn stats(&self) -> PluginStats {
        let state = self.store.data();
        PluginStats {
            calls: state.counters.calls,
            fuel_consumed: state.counters.fuel_consumed,
            peak_memory_bytes: state.limits.peak_memory as u64,
            log_messages: state.counters.log_messages,
            log_bytes: state.counters.log_bytes,
            imports_declared: self.report.decisions.len(),
            imports_denied: self.report.denied().count(),
            reloads: state.counters.reloads,
        }
    }

    /// Replace this plugin's code with new bytes, keeping its name and slot.
    ///
    /// Reload is [`Host::load`] plus a state handoff, **not** a cheaper path
    /// that skips the checks. The bytes are compiled and their imports
    /// intersected with the same manifest, and a component that asks for
    /// something the manifest does not grant is refused here exactly as it
    /// would be at load: new bytes must not be able to acquire a capability by
    /// arriving as an update. That is ADR-0010's central claim.
    ///
    /// # Nothing is replaced until everything has succeeded
    ///
    /// The replacement is built to completion first — compiled, checked,
    /// instantiated, and given the state — and only then does it take over.
    /// Every failure path returns before that, leaving the plugin *running the
    /// code it was already running*: a plugin that cannot be replaced is better
    /// than a host left with none. Two instances therefore exist at once for
    /// the duration of the call, which for a large guest is a real spike.
    ///
    /// One caveat, stated rather than glossed. Every failure that can be
    /// reached without calling the guest — a refused import, bytes that are not
    /// a component, a hook of a shape the host cannot call, a state type the
    /// replacement does not take — happens before anything is entered, and
    /// leaves a fully working plugin. The exception is a *trap inside
    /// `save-state`*: Wasmtime 48 refuses to re-enter a component instance
    /// after any trap, so that plugin is unreplaced but also uncallable. Reload
    /// neither causes that nor can undo it — a trapping ordinary call does the
    /// same — and it is the reason the replacement is built first.
    ///
    /// # State
    ///
    /// If the outgoing instance exports [`SAVE_STATE_EXPORT`] it is called, and
    /// the value it returns is passed to the incoming instance's
    /// [`RESTORE_STATE_EXPORT`]. Both are optional and either side may be
    /// missing, in which case the reload happens with no state rather than
    /// failing. The value crosses as a typed WIT value: it is bounded by
    /// `limits.transfer` like any other crossing, it goes through the seam a
    /// [`TraceHook`] watches, and its type must match on both sides — this is
    /// not cross-version state migration, which the spec puts on the list of
    /// things watoots does not build.
    ///
    /// Both hooks are ordinary calls: they burn fuel, count towards
    /// [`Plugin::stats`], and appear in a trace.
    ///
    /// # Counters
    ///
    /// [`Plugin::stats`] accumulates across a reload; [`Plugin::profile`]
    /// starts again. Each has its own reason, given on each of them. Sampled
    /// guest profiles do not survive at all — the sampler is bound to one
    /// compiled component — so call [`Plugin::write_guest_profile`] before
    /// reloading if you want them.
    pub fn reload(&mut self, wasm: &[u8]) -> Result<ReloadReport> {
        let vars = self.vars.clone();
        self.reload_with(wasm, vars)
    }

    /// Replace this plugin's code with a component read from disk.
    ///
    /// As [`Plugin::reload`], and additionally re-points `${plugin_dir}` at the
    /// directory the replacement came from. The plugin keeps the name it was
    /// registered under even when the file is named differently — a reload
    /// replaces code, not identity.
    pub fn reload_from_file(&mut self, path: impl AsRef<Path>) -> Result<ReloadReport> {
        let path = path.as_ref();
        let wasm = read_component(path)?;
        let mut vars = self.vars.clone();
        vars.extend(plugin_dir_var(path));
        self.reload_with(&wasm, vars)
    }

    /// The one place a running plugin is replaced.
    ///
    /// Written as four steps that each either fail or produce a value, and a
    /// single assignment at the end that cannot fail. Anything added here
    /// belongs *above* that assignment; nothing below it is allowed to be
    /// fallible, which is what keeps "the old instance survives" a property of
    /// the shape rather than of remembering to be careful.
    fn reload_with(&mut self, wasm: &[u8], vars: BTreeMap<String, String>) -> Result<ReloadReport> {
        // Hashed here as well as inside `instantiate_plugin`, and only when a
        // hook is installed: the reload's own verdict is reached after that
        // function has returned, and "which bytes am I running now" is the whole
        // point of recording a reload.
        let digest = self
            .audit
            .as_ref()
            .map_or_else(String::new, |_| sha256_hex(wasm));

        // 1. Build the replacement, through the same function `Host::load`
        //    uses: the same compile, the same import intersection, the same
        //    refusal. Reload has no load path of its own to forget a check in,
        //    and a check added to loading is a check reload gains for free.
        //
        //    First, deliberately: a reload refused for asking too much has then
        //    not so much as called into the running plugin. That refusal is
        //    reported from in there, where the deciding import is known.
        let mut fresh = self
            .host
            .instantiate_plugin(&self.name, wasm, vars, LoadKind::Reload)?;

        // 2. Ask the outgoing instance for its state, if it has any to give.
        let saved = match self.save_state() {
            Ok(saved) => saved,
            Err(err) => return Err(self.audit_reload_refused(&digest, err)),
        };

        // 3. Hand it to the replacement, if the replacement takes it.
        let state_restored = match &saved {
            None => false,
            Some(state) => match fresh.restore_state(state) {
                Ok(restored) => restored,
                Err(err) => return Err(self.audit_reload_refused(&digest, err)),
            },
        };

        // 4. Nothing after this line can fail.
        fresh.inherit(self);
        let reloads = fresh.store.data().counters.reloads;
        *self = fresh;

        // Said here rather than where the replacement was built, because until
        // this point the reload could still have been sent back.
        if let Some(hook) = &self.audit {
            hook.on_event(&AuditEvent::Reloaded {
                plugin: &self.name,
                sha256: &digest,
                reloads,
            });
        }

        Ok(ReloadReport {
            state_saved: saved.is_some(),
            state_restored,
            reloads,
        })
    }

    /// Report a reload refused *after* the replacement had already been built
    /// and granted — a trap in `save-state`, or a state type that does not line
    /// up. No import decided these, so none is named.
    fn audit_reload_refused(&self, sha256: &str, err: Error) -> Error {
        if let Some(hook) = &self.audit {
            hook.on_event(&AuditEvent::ReloadRefused {
                plugin: &self.name,
                sha256,
                import: None,
                requirement: None,
                reason: err.message(),
            });
        }
        err
    }

    /// Call `save-state`, or report that there is none to call.
    ///
    /// The type comes back with the value because the incoming instance's
    /// `restore-state` has to be checked against it, and a mismatch deserves to
    /// be named rather than surfacing as the canonical ABI refusing an
    /// argument.
    fn save_state(&mut self) -> Result<Option<(Val, Type)>> {
        let Some(func) = self.instance.get_func(&mut self.store, SAVE_STATE_EXPORT) else {
            return Ok(None);
        };

        let ty = func.ty(&self.store);
        let results: Vec<Type> = ty.results().collect();
        if ty.params().len() != 0 || results.len() != 1 {
            return Err(Error::new(
                ErrorKind::InvalidArgument,
                format!(
                    "{}: {SAVE_STATE_EXPORT} must take no parameters and return \
                     exactly one value; this one takes {} and returns {}",
                    self.name,
                    ty.params().len(),
                    results.len()
                ),
            ));
        }

        let mut values = self.call(SAVE_STATE_EXPORT, &[])?;
        let value = values.pop().ok_or_else(|| {
            Error::new(
                ErrorKind::Internal,
                format!("{}: {SAVE_STATE_EXPORT} returned nothing", self.name),
            )
        })?;
        Ok(Some((
            value,
            results.into_iter().next().expect("one result"),
        )))
    }

    /// Give `restore-state` the value, or report that there is nowhere to put
    /// it.
    fn restore_state(&mut self, state: &(Val, Type)) -> Result<bool> {
        let (value, saved_type) = state;
        let Some(func) = self
            .instance
            .get_func(&mut self.store, RESTORE_STATE_EXPORT)
        else {
            return Ok(false);
        };

        let ty = func.ty(&self.store);
        let params: Vec<Type> = ty.params().map(|(_, ty)| ty).collect();
        if params.len() != 1 || ty.results().len() != 0 {
            return Err(Error::new(
                ErrorKind::InvalidArgument,
                format!(
                    "{}: {RESTORE_STATE_EXPORT} must take exactly one parameter \
                     and return nothing; this one takes {} and returns {}",
                    self.name,
                    params.len(),
                    ty.results().len()
                ),
            ));
        }

        if params[0] != *saved_type {
            return Err(Error::new(
                ErrorKind::InvalidArgument,
                format!(
                    "{}: the outgoing build's {SAVE_STATE_EXPORT} returns {:?} \
                     but the replacement's {RESTORE_STATE_EXPORT} takes {:?}. \
                     State crosses a reload as a typed value of one shape; \
                     changing that shape is changing the world, and watoots \
                     does not migrate state between versions of it.",
                    self.name, saved_type, params[0]
                ),
            ));
        }

        self.call(RESTORE_STATE_EXPORT, std::slice::from_ref(value))?;
        Ok(true)
    }

    /// Take over the outgoing instance's lifetime totals.
    ///
    /// Called on the *replacement*, immediately before it takes over, and
    /// infallible on purpose: by the time this runs there is nothing left that
    /// could send the reload back. See [`PluginStats`] for why the counters
    /// accumulate here and the profile does not.
    fn inherit(&mut self, previous: &Self) {
        let carried = previous.store.data().counters;
        let peak = previous.store.data().limits.peak_memory;

        let state = self.store.data_mut();
        state.counters.calls += carried.calls;
        state.counters.fuel_consumed += carried.fuel_consumed;
        state.counters.log_messages += carried.log_messages;
        state.counters.log_bytes += carried.log_bytes;
        state.counters.reloads = carried.reloads + 1;
        // A high-water mark across the plugin's life, which is what the field
        // says it is. The replacement has only just started, so without this
        // the number would fall — and a peak that falls is not a peak.
        state.limits.peak_memory = state.limits.peak_memory.max(peak);
    }

    /// Where this plugin's time has gone, split at the boundary.
    ///
    /// The sibling of [`Plugin::stats`]: both are observed at the crossing
    /// rather than reported by the guest, and this one answers "where" rather
    /// than "how much". See [`PluginProfile`] for what each bucket covers, and
    /// in particular for why `marshalling_nanos` is a remainder rather than a
    /// measurement.
    ///
    /// Fails when the host was not built with
    /// [`HostBuilder::profile`](crate::HostBuilder::profile), because a page of
    /// zeroes is a worse answer than being told the feature is off.
    ///
    /// # Across a reload
    ///
    /// This **starts again** at every [`Plugin::reload`], which is the opposite
    /// of what [`Plugin::stats`] does and for the opposite reason: a profile
    /// attributes time to *code*, and one row averaging two different builds of
    /// an export describes neither. Comparing the profile before a reload with
    /// the profile after is the whole reason to profile a reload at all, and
    /// summing them would destroy it. `profile().calls` therefore drifts below
    /// `stats().calls` on a plugin that has been reloaded, which is the visible
    /// consequence of the two answering different questions.
    pub fn profile(&self) -> Result<PluginProfile> {
        self.store.data().profile.as_ref().map_or_else(
            || {
                Err(Error::new(
                    ErrorKind::InvalidArgument,
                    format!(
                        "{}: profiling is not enabled; build the host with \
                         HostBuilder::profile()",
                        self.name
                    ),
                ))
            },
            |state| Ok(state.report()),
        )
    }

    /// Write the sampled guest profile as Firefox Profiler JSON.
    ///
    /// This is the half of ADR-0009 that is Wasmtime's: `GuestProfiler` answers
    /// "which guest function is hot", which [`Plugin::profile`] cannot, because
    /// it never looks inside the guest. Load the file at
    /// <https://profiler.firefox.com/>.
    ///
    /// Needs [`HostBuilder::profile_guest_samples`](crate::HostBuilder::profile_guest_samples).
    /// The profiler is consumed: sampling stops here, and a second call fails.
    pub fn write_guest_profile(&mut self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        let profiler = self
            .store
            .data_mut()
            .profile
            .as_mut()
            .and_then(|state| state.guest.take())
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidArgument,
                    format!(
                        "{}: no guest samples were collected; build the host with \
                         HostBuilder::profile_guest_samples(interval), and write \
                         the profile only once",
                        self.name
                    ),
                )
            })?;

        let file = std::fs::File::create(path).map_err(|err| {
            Error::new(
                ErrorKind::InvalidArgument,
                format!("cannot write {}: {err}", path.display()),
            )
        })?;
        profiler
            .finish(std::io::BufWriter::new(file))
            .map_err(|err| {
                Error::new(
                    ErrorKind::Internal,
                    format!("{}: writing {}: {err:?}", self.name, path.display()),
                )
            })
    }

    /// Fold the call that just finished into the lifetime totals.
    ///
    /// Fuel and the log budget are per call and re-armed by [`arm`], so they
    /// have to be read before the next call resets them.
    fn settle(&mut self) {
        // `get_fuel` errors when the store is not metered, which is exactly the
        // case where nothing was burned.
        let remaining = self.store.get_fuel().unwrap_or(0);
        let burned = self
            .limits
            .fuel
            .map_or(0, |budget| budget.saturating_sub(remaining));
        let log = self.store.data().log.used();
        let counters = &mut self.store.data_mut().counters;
        counters.calls += 1;
        counters.fuel_consumed += burned;
        counters.log_messages += log.0;
        counters.log_bytes += log.1;
    }

    /// Call an exported function by name.
    ///
    /// Untyped on purpose: this is the path the C API and the CLI take, and the
    /// one the recorder sits on. Rust hosts with a static world can reach for
    /// `bindgen!` against the same engine instead.
    ///
    /// Fuel and the deadline are reset before each call, so `fuel` and
    /// `timeout` in the manifest are per-call budgets rather than per-plugin
    /// ones.
    pub fn call(&mut self, export: &str, args: &[Val]) -> Result<Vec<Val>> {
        // The wall clock for the marshalling remainder starts here, before the
        // export lookup, because the remainder is honest about absorbing our own
        // dispatch overhead rather than quietly excluding it.
        let started = self.profiling.then(Instant::now);

        let func = self
            .instance
            .get_func(&mut self.store, export)
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::NotFound,
                    format!("{}: no exported function {export:?}", self.name),
                )
            })?;

        let result_count = func.ty(&self.store).results().len();
        let mut results = vec![Val::Bool(false); result_count];

        arm(&mut self.store, &self.limits, &self.name)?;

        if let Some(hook) = &self.trace {
            hook.on_event(&TraceEvent::ExportCall {
                plugin: &self.name,
                func: export,
                args,
            });
        }

        let outcome = func
            .call(&mut self.store, args, &mut results)
            .map_err(|err| self.classify_call_error(export, &err));

        // Before the early return below: a call that trapped burned fuel and
        // may have logged, and those are exactly the calls an operator is
        // trying to account for. A call that trapped also spent time, and the
        // same argument applies.
        if let Some(started) = started {
            let wall = started.elapsed();
            if let Some(profile) = self.store.data_mut().profile.as_mut() {
                profile.finish_call(export, wall);
            }
        }
        self.settle();

        if let Some(hook) = &self.trace {
            let reported = match &outcome {
                Ok(()) => Outcome::Returned(&results),
                Err(err) => Outcome::Failed(err),
            };
            hook.on_event(&TraceEvent::ExportReturn {
                plugin: &self.name,
                func: export,
                outcome: reported,
            });
        }

        outcome?;
        Ok(results)
    }

    /// The parameter types of an exported function, as the component declares
    /// them.
    ///
    /// This is what makes a generated call type-correct by construction rather
    /// than by luck: `watoots fuzz` builds each argument from the type the
    /// world says it has, so nothing is spent being rejected by the canonical
    /// ABI. See `docs/adr/0008-fuzzing.md`.
    pub fn export_params(&mut self, export: &str) -> Result<Vec<Type>> {
        let func = self
            .instance
            .get_func(&mut self.store, export)
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::NotFound,
                    format!("{}: no exported function {export:?}", self.name),
                )
            })?;
        Ok(func.ty(&self.store).params().map(|(_, ty)| ty).collect())
    }

    /// Call an exported function, taking and returning WAVE text.
    ///
    /// This is the path a CLI or a C caller takes: it has strings, not `Val`s,
    /// and the function's own type is what says how to read them. `"notes.md"`
    /// parses as a string because the world says the parameter is a string.
    pub fn call_wave(&mut self, export: &str, args: &[&str]) -> Result<Vec<String>> {
        let params: Vec<wasmtime::component::Type> = {
            let func = self
                .instance
                .get_func(&mut self.store, export)
                .ok_or_else(|| {
                    Error::new(
                        ErrorKind::NotFound,
                        format!("{}: no exported function {export:?}", self.name),
                    )
                })?;
            func.ty(&self.store).params().map(|(_, ty)| ty).collect()
        };

        if args.len() != params.len() {
            return Err(Error::new(
                ErrorKind::InvalidArgument,
                format!(
                    "{}: {export} takes {} argument(s), got {}",
                    self.name,
                    params.len(),
                    args.len()
                ),
            ));
        }

        // Timed separately from the call itself. `Plugin::call` starts its
        // clock inside itself, so parsing here and rendering below both fall
        // outside it — which made a 177ms call report 52ms and charge the
        // difference to marshalling, a bucket that had nothing to do with it.
        let parsing = self.profiling.then(Instant::now);
        let values = args
            .iter()
            .zip(&params)
            .map(|(text, ty)| crate::wave::from_wave(ty, text))
            .collect::<Result<Vec<_>>>()?;
        let parsed = parsing.map(|at| at.elapsed());

        let results = self.call(export, &values)?;

        let rendering = self.profiling.then(Instant::now);
        let text: Result<Vec<String>> = results.iter().map(crate::wave::to_wave).collect();
        if let Some(parsed) = parsed {
            let spent = parsed + rendering.map_or(Duration::ZERO, |at| at.elapsed());
            if let Some(profile) = self.store.data_mut().profile.as_mut() {
                profile.add_wave(export, u64::try_from(spent.as_nanos()).unwrap_or(u64::MAX));
            }
        }
        text
    }

    /// Separate "the plugin misbehaved" from "the plugin hit a ceiling", since
    /// the two mean different things to whoever installed it.
    ///
    /// The message leads with the root cause and puts the guest backtrace
    /// underneath. Wasmtime's own `Debug` rendering does the opposite, which
    /// buries "all fuel consumed" below a stack trace — no use to a C caller
    /// that gets one string, and not much better in a log line.
    ///
    /// A store limiter refusing growth makes `memory.grow` return -1 rather
    /// than raise an error, so a memory ceiling is only ever reported here via
    /// a trap; there is nothing extra to match on.
    fn classify_call_error(&self, export: &str, err: &wasmtime::Error) -> Error {
        // The same match answers both questions, so the audit trail cannot come
        // to a different conclusion about a ceiling than the error does. The log
        // budgets are the exception: the shim has already reported which of the
        // two was spent, and it is the only thing that knows.
        let (kind, ceiling) = if err.downcast_ref::<LogVolumeExceeded>().is_some() {
            (ErrorKind::LimitExceeded, None)
        } else if exhausted_transfer_budget(err) {
            (ErrorKind::LimitExceeded, Some(Ceiling::Transfer))
        } else {
            match err.downcast_ref::<wasmtime::Trap>() {
                Some(wasmtime::Trap::OutOfFuel) => (ErrorKind::LimitExceeded, Some(Ceiling::Fuel)),
                Some(wasmtime::Trap::Interrupt) => {
                    (ErrorKind::LimitExceeded, Some(Ceiling::Timeout))
                }
                _ => (ErrorKind::Trap, None),
            }
        };

        if let (Some(hook), Some(limit)) = (&self.audit, ceiling) {
            hook.on_event(&AuditEvent::CeilingSpent {
                plugin: &self.name,
                limit,
            });
        }

        Error::new(
            kind,
            format!("{}: {export}: {}\n{err}", self.name, err.root_cause()),
        )
    }
}

/// Whether an error is `limits.transfer` being spent, rather than a plugin
/// misbehaving.
///
/// Matched on the message, which is not how anything else here is classified —
/// wasmtime raises this as a private `HostcallFuelExhausted` with no public
/// type to downcast to, so the rendering is the only signal there is.
///
/// The fragility is real and is answered by a test rather than by accepting the
/// wrong answer: `a_transfer_overrun_is_a_limit_and_not_a_trap` fails loudly if
/// wasmtime ever rewords this. Reporting a ceiling as a trap is worse than
/// depending on a string — it sends whoever installed the plugin to debug the
/// plugin, when what they need to edit is their own manifest.
fn exhausted_transfer_budget(err: &wasmtime::Error) -> bool {
    err.chain()
        .any(|cause| cause.to_string().contains("fuel allocated for hostcalls"))
}

/// Install `wasi:logging` on the linker, if the manifest grants it.
///
/// Not routed through [`install_host_funcs`] on purpose. Registering it as a
/// host function would put `wasi:logging/logging` in the host-provided set, and
/// `classify` consults that set before it looks at the WASI table — so an
/// application would silently shadow the capability check and the manifest's
/// `logging` grant would stop meaning anything. Keeping the shim separate keeps
/// [`crate::imports::Requirement::Logging`] the only thing that admits it.
fn install_logging(linker: &mut Linker<State>, plugin: &str, wiring: &Wiring<'_>) -> Result<()> {
    // Absence denies. The load-time check has already refused any component
    // that imports the interface, so there is nothing to link.
    let Some(ceiling) = wiring.manifest.permissions.logging else {
        return Ok(());
    };

    for interface in [LOGGING_VERSIONED, LOGGING_UNVERSIONED] {
        let sink = wiring.log_sink.map(Arc::clone);
        let trace = wiring.trace.map(Arc::clone);
        let audit = wiring.audit.map(Arc::clone);
        let owned_plugin = plugin.to_string();

        let mut instance = linker.instance(interface).map_err(|err| {
            Error::new(
                ErrorKind::Internal,
                format!("{plugin}: cannot define interface {interface:?}: {err:?}"),
            )
        })?;

        instance
            .func_new(LOGGING_FUNC, move |mut store, _ty, params, _results| {
                // The call hook opened a host window when the guest crossed;
                // only the shim knows whose it is. See `ProfileState::serving`.
                if let Some(profile) = store.data_mut().profile.as_mut() {
                    profile.serving(interface, LOGGING_FUNC);
                }

                // Recorded before the ceiling filters, so a trace is the
                // plugin's full account of what it thought was happening rather
                // than the subset this host's policy chose to print.
                if let Some(hook) = &trace {
                    hook.on_event(&TraceEvent::ImportCall {
                        plugin: &owned_plugin,
                        interface,
                        func: LOGGING_FUNC,
                        args: params,
                    });
                }

                let outcome = deliver_log(
                    store.data_mut(),
                    ceiling,
                    sink.as_ref(),
                    audit.as_ref(),
                    &owned_plugin,
                    params,
                );

                if let Some(hook) = &trace {
                    let reported = match &outcome {
                        Ok(()) => Outcome::Returned(&[]),
                        Err(err) => Outcome::Failed(err),
                    };
                    hook.on_event(&TraceEvent::ImportReturn {
                        plugin: &owned_plugin,
                        interface,
                        func: LOGGING_FUNC,
                        outcome: reported,
                    });
                }

                match outcome {
                    Ok(()) => Ok(()),
                    // A ceiling, not a misbehaviour: the marker survives the
                    // backtrace wasmtime layers on, so the caller sees
                    // LimitExceeded rather than Trap.
                    Err(err) if err.kind() == ErrorKind::LimitExceeded => Err(
                        wasmtime::Error::new(LogVolumeExceeded(err.message().to_string())),
                    ),
                    Err(err) => Err(wasmtime::Error::msg(err.message().to_string())),
                }
            })
            .map_err(|err| {
                Error::new(
                    ErrorKind::Internal,
                    format!("{plugin}: cannot define {interface}#{LOGGING_FUNC}: {err:?}"),
                )
            })?;
    }
    Ok(())
}

/// Charge one `log` call against the budget, filter it, and hand it on.
///
/// Every branch out of here is an audit event, and none of them carries the
/// message. That is the constraint ADR-0011 is built around: "the plugin said
/// nothing" and "the plugin was not allowed to say it" mean opposite things and
/// have until now been indistinguishable — but a line kept for an incident must
/// be one you can paste into an issue, and the guest's text is not.
fn deliver_log(
    state: &mut State,
    ceiling: LogLevel,
    sink: Option<&LogSink>,
    audit: Option<&Arc<dyn AuditHook>>,
    plugin: &str,
    params: &[Val],
) -> Result<()> {
    let [Val::Enum(level), Val::String(context), Val::String(message)] = params else {
        return Err(Error::new(
            ErrorKind::Internal,
            format!(
                "{plugin}: {LOGGING_VERSIONED}#{LOGGING_FUNC} was called with \
                 {} argument(s) of an unexpected shape; \
                 this host implements wasi:logging@0.1.0-draft",
                params.len()
            ),
        ));
    };

    let bytes = (context.len() as u64).saturating_add(message.len() as u64);
    state.log.charge(bytes).map_err(|(limit, why)| {
        if let Some(hook) = audit {
            hook.on_event(&AuditEvent::CeilingSpent { plugin, limit });
        }
        Error::new(ErrorKind::LimitExceeded, format!("{plugin}: {why}"))
    })?;

    let Some(level) = LogLevel::from_wit_name(level) else {
        // A case this build does not know cannot be compared against the
        // ceiling, so it cannot be shown to satisfy it. Drop rather than
        // deliver: the manifest's ceiling has to hold across a revision of a
        // Phase 1 proposal that adds a case.
        if let Some(hook) = audit {
            hook.on_event(&AuditEvent::LogSuppressed {
                plugin,
                level: None,
                ceiling,
            });
        }
        return Ok(());
    };

    if level < ceiling {
        if let Some(hook) = audit {
            hook.on_event(&AuditEvent::LogSuppressed {
                plugin,
                level: Some(level),
                ceiling,
            });
        }
        return Ok(());
    }

    // Admitted by the manifest, which is the decision. Whether it then reaches
    // anybody is a separate question: a grant with no sink links and discards.
    if let Some(hook) = audit {
        hook.on_event(&AuditEvent::LogAdmitted { plugin, level });
    }

    if let Some(sink) = sink {
        sink(&LogRecord {
            level,
            context,
            message,
        });
    }
    Ok(())
}

/// Install the application's own interfaces on the linker.
///
/// These go in as dynamically typed functions rather than through `bindgen!`,
/// for the same reason [`Plugin::call`] is untyped: it is the shape the C API
/// can express, and it is the shape a recorder can serialize without knowing
/// the world in advance.
fn install_host_funcs(linker: &mut Linker<State>, plugin: &str, wiring: &Wiring<'_>) -> Result<()> {
    for (interface, funcs) in wiring.host_funcs {
        let mut instance = linker.instance(interface).map_err(|err| {
            Error::new(
                ErrorKind::Internal,
                format!("{plugin}: cannot define interface {interface:?}: {err:?}"),
            )
        })?;

        for (func_name, host_func) in funcs {
            // Owned copies for the closure; the originals stay available for
            // the error message below.
            let host_func = Arc::clone(host_func);
            let trace = wiring.trace.map(Arc::clone);
            let owned_plugin = plugin.to_string();
            let owned_interface = interface.clone();
            let owned_func = func_name.clone();

            instance
                .func_new(func_name, move |mut store, ty, params, results| {
                    // See the logging shim: the call hook timed the crossing,
                    // the shim names it.
                    if let Some(profile) = store.data_mut().profile.as_mut() {
                        profile.serving(&owned_interface, &owned_func);
                    }

                    if let Some(hook) = &trace {
                        hook.on_event(&TraceEvent::ImportCall {
                            plugin: &owned_plugin,
                            interface: &owned_interface,
                            func: &owned_func,
                            args: params,
                        });
                    }

                    // The world's own declaration of what this function
                    // returns, so a text-based host can answer in the right
                    // type rather than guessing from the literal.
                    let result_types: Vec<wasmtime::component::Type> = ty.results().collect();
                    let outcome = host_func(&crate::host::HostCall {
                        args: params,
                        result_types: &result_types,
                    });

                    if let Some(hook) = &trace {
                        let reported = match &outcome {
                            Ok(values) => Outcome::Returned(values),
                            Err(err) => Outcome::Failed(err),
                        };
                        hook.on_event(&TraceEvent::ImportReturn {
                            plugin: &owned_plugin,
                            interface: &owned_interface,
                            func: &owned_func,
                            outcome: reported,
                        });
                    }

                    let values = outcome.map_err(|err| {
                        wasmtime::Error::msg(format!(
                            "{owned_plugin}: host function {owned_interface}#{owned_func}: {}",
                            err.message()
                        ))
                    })?;

                    if values.len() != results.len() {
                        return Err(wasmtime::Error::msg(format!(
                            "{owned_plugin}: host function {owned_interface}#{owned_func} \
                             returned {} value(s), the world declares {}",
                            values.len(),
                            results.len()
                        )));
                    }
                    results.clone_from_slice(&values);
                    Ok(())
                })
                .map_err(|err| {
                    Error::new(
                        ErrorKind::Internal,
                        format!("{plugin}: cannot define {interface}#{func_name}: {err:?}"),
                    )
                })?;
        }
    }
    Ok(())
}

/// Reset the per-call budgets on a store.
fn arm(store: &mut Store<State>, limits: &Limits, name: &str) -> Result<()> {
    store.data_mut().log.rearm();
    // Per crossing rather than per call, so wasmtime resets it itself; this
    // sets the value each one starts from. Armed here with the others so a
    // manifest is the only place any ceiling comes from.
    store.set_hostcall_fuel(usize::try_from(limits.transfer).unwrap_or(usize::MAX));
    if let Some(fuel) = limits.fuel {
        store.set_fuel(fuel).map_err(|err| {
            Error::new(
                ErrorKind::Internal,
                format!("{name}: cannot set fuel: {err:?}"),
            )
        })?;
    }
    // Deadlines are counted in epoch ticks; see EPOCH_TICK in host.rs. The
    // budget knows about both `limits.timeout` and the sampling interval, and
    // hands out the shorter of the two — see `Deadline`.
    if let Some(ticks) = store.data_mut().deadline.rearm() {
        store.set_epoch_deadline(ticks);
    }
    Ok(())
}

/// Build the Firefox-profile sampler, when one was asked for.
///
/// `GuestProfiler::new_component` needs the component's core modules, so this
/// takes the `Component` rather than the bytes — including one deserialized
/// from the `.cwasm` cache, which carries its modules just as a freshly
/// compiled one does.
fn build_sampler(
    name: &str,
    engine: &Engine,
    component: &Component,
    options: Profiling,
) -> Result<Option<GuestProfiler>> {
    let Some(interval) = options.sample_interval else {
        return Ok(None);
    };
    GuestProfiler::new_component(engine, name, interval, component.clone(), [])
        .map(Some)
        .map_err(|err| {
            Error::new(
                ErrorKind::Internal,
                format!("{name}: cannot start the guest profiler: {err:?}"),
            )
        })
}

/// Observe every host↔guest transition, for the three buckets and for the
/// sampled profile's host-call markers.
fn install_profiler(store: &mut Store<State>) {
    store.call_hook(|mut store, kind| {
        // Taken out and put back so the profiler can be handed the store it is
        // reading stacks from; wasmtime does the same dance with the epoch
        // callback, and for the same borrow reason.
        let mut guest = store
            .data_mut()
            .profile
            .as_mut()
            .and_then(|p| p.guest.take());
        if let Some(profiler) = &mut guest {
            profiler.call_hook(&store, kind);
        }
        if let Some(profile) = store.data_mut().profile.as_mut() {
            profile.guest = guest;
            profile.on_call_hook(kind);
        }
        Ok(())
    });
}

/// Share the epoch deadline between sampling and the timeout.
///
/// **The timeout wins.** Sampling is the reason the callback exists, but the
/// callback is on the path that stops a runaway plugin, so it samples and then
/// asks [`Deadline`] what is left of the budget: a slice while there is one, a
/// trap once there is not. Extending the deadline past the budget here would
/// defeat the thing this project is for.
fn install_epoch_callback(store: &mut Store<State>) {
    store.epoch_deadline_callback(|mut store| {
        let mut guest = store
            .data_mut()
            .profile
            .as_mut()
            .and_then(|p| p.guest.take());
        if let Some(profiler) = &mut guest {
            let delta = store
                .data_mut()
                .profile
                .as_mut()
                .map_or(std::time::Duration::ZERO, ProfileState::sample_delta);
            profiler.sample(&store, delta);
        }
        let state = store.data_mut();
        if let Some(profile) = state.profile.as_mut() {
            profile.guest = guest;
        }

        Ok(match state.deadline.next() {
            Some(ticks) => UpdateDeadline::Continue(ticks),
            None => UpdateDeadline::Interrupt,
        })
    });
}

/// Turn manifest grants into a WASI context.
///
/// Only the filesystem and environment need building here. Clocks and
/// randomness are denied by *not being importable*: the intersection check
/// refuses to load a component that imports an interface the manifest does not
/// grant, so there is nothing to switch off afterwards.
///
/// Sockets are the exception, and the reason the two mechanisms both exist. A
/// CPython or JavaScript guest links the socket interfaces whether or not the
/// plugin opens one, so `net = []` grants the *import* while wasmtime-wasi 48's
/// own defaults refuse every connection. We never call `allow_tcp` or
/// `allow_udp`, so that stays true.
fn build_wasi_ctx(manifest: &Manifest) -> Result<WasiCtx> {
    let permissions = &manifest.permissions;
    let mut builder = WasiCtx::builder();

    // Pinned clocks and a seeded generator. A plugin that reads the time or
    // asks for randomness still gets an answer; it gets the *same* answer on
    // every run, which is what makes a recording replayable somewhere else.
    let determinism = &manifest.determinism;
    if determinism.enabled {
        builder.wall_clock(PinnedWallClock {
            at: std::time::Duration::from_secs(determinism.epoch_seconds),
        });
        builder.monotonic_clock(SteppingClock {
            step: determinism.monotonic_step_nanos.max(1),
            now: std::sync::atomic::AtomicU64::new(0),
        });
        builder.secure_random(wasmtime_wasi::Deterministic::new(
            determinism.seed.as_bytes().to_vec(),
        ));
        builder.insecure_random(wasmtime_wasi::Deterministic::new(
            determinism.seed.as_bytes().to_vec(),
        ));
        builder.insecure_random_seed(u128::from(determinism.epoch_seconds));
    }

    for path in &permissions.fs.read {
        preopen(&mut builder, path, FsPerms::ReadOnly)?;
    }
    for path in &permissions.fs.write {
        preopen(&mut builder, path, FsPerms::ReadWrite)?;
    }

    if let Some(env) = &permissions.env {
        let pairs: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        builder.envs(&pairs);
    }

    Ok(builder.build())
}

/// A wall clock that always reads the same instant.
struct PinnedWallClock {
    at: std::time::Duration,
}

impl wasmtime_wasi::HostWallClock for PinnedWallClock {
    fn resolution(&self) -> std::time::Duration {
        std::time::Duration::from_secs(1)
    }

    fn now(&self) -> std::time::Duration {
        self.at
    }
}

/// A monotonic clock that advances a fixed amount per read.
///
/// It has to move: a guest polling for a deadline against a frozen clock spins
/// forever. It has to move predictably, or the recording does not reproduce.
struct SteppingClock {
    step: u64,
    now: std::sync::atomic::AtomicU64,
}

impl wasmtime_wasi::HostMonotonicClock for SteppingClock {
    fn resolution(&self) -> u64 {
        self.step
    }

    fn now(&self) -> u64 {
        self.now
            .fetch_add(self.step, std::sync::atomic::Ordering::Relaxed)
    }
}

fn preopen(builder: &mut wasmtime_wasi::WasiCtxBuilder, path: &str, perms: FsPerms) -> Result<()> {
    builder.preopened_dir(path, path, perms).map_err(|err| {
        Error::new(
            ErrorKind::Manifest,
            format!("cannot preopen {path:?} granted by the manifest: {err}"),
        )
    })?;
    Ok(())
}
