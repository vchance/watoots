//! A loaded, instantiated component and the store it runs in.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use wasmtime::component::{Component, Linker, ResourceTable, Type, Val};
use wasmtime::{Engine, GuestProfiler, Store, StoreLimits, StoreLimitsBuilder, UpdateDeadline};
use wasmtime_wasi::{FsPerms, WasiCtx, WasiCtxView, WasiView};

use crate::host::{HostFunc, LogRecord, LogSink, epoch_ticks};
use crate::imports::GrantReport;
use crate::manifest::{Limits, LogLevel, Manifest};
use crate::profile::{Deadline, PluginProfile, ProfileState, Profiling};
use crate::trace::{Outcome, TraceEvent, TraceHook};
use crate::{Error, ErrorKind, Result};

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
}

/// What a host has observed about one plugin.
///
/// The half of "metrics" worth having, and the reason ADR-0006 declines to let
/// a plugin report its own: these numbers are observed at the boundary, so a
/// guest cannot inflate or forge them, and their cardinality is fixed by this
/// struct rather than by untrusted input.
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
    pub imports_declared: usize,
    /// How many of those the manifest did not grant. Non-zero only where the
    /// application served them itself, since otherwise the load would have
    /// failed.
    pub imports_denied: usize,
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

    /// Charge one message, or say why it does not fit.
    ///
    /// Charged before the level ceiling filters, deliberately: what this bounds
    /// is the work the *host* does lifting a string out of guest memory, and
    /// that has already happened by the time we can read the level. Charging
    /// only what survives the filter would let `logging = "critical"` be a
    /// licence to push unbounded bytes across the boundary at `trace`.
    fn charge(&mut self, bytes: u64) -> std::result::Result<(), String> {
        self.messages_used = self.messages_used.saturating_add(1);
        self.bytes_used = self.bytes_used.saturating_add(bytes);

        if self.messages_used > self.messages_allowed {
            return Err(format!(
                "log message limit exceeded: {} message(s) in one call, limits.log_messages is {}",
                self.messages_used, self.messages_allowed
            ));
        }
        if self.bytes_used > self.bytes_allowed {
            return Err(format!(
                "log volume limit exceeded: {} byte(s) in one call, limits.log_bytes is {}",
                self.bytes_used, self.bytes_allowed
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
    /// Mirrors `State::profile.is_some()`, so the hot path in [`Plugin::call`]
    /// costs one bool rather than a store borrow.
    profiling: bool,
}

impl fmt::Debug for Plugin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Plugin")
            .field("name", &self.name)
            .field("limits", &self.limits)
            .field("imports", &self.report.decisions.len())
            .finish_non_exhaustive()
    }
}

impl Plugin {
    pub(crate) fn instantiate(
        name: &str,
        engine: &Engine,
        component: &Component,
        wiring: &Wiring<'_>,
        report: GrantReport,
    ) -> Result<Self> {
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
            profiling,
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
        }
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

        let values = args
            .iter()
            .zip(&params)
            .map(|(text, ty)| crate::wave::from_wave(ty, text))
            .collect::<Result<Vec<_>>>()?;

        self.call(export, &values)?
            .iter()
            .map(crate::wave::to_wave)
            .collect()
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
        let kind = if err.downcast_ref::<LogVolumeExceeded>().is_some() {
            ErrorKind::LimitExceeded
        } else {
            match err.downcast_ref::<wasmtime::Trap>() {
                Some(wasmtime::Trap::OutOfFuel | wasmtime::Trap::Interrupt) => {
                    ErrorKind::LimitExceeded
                }
                _ => ErrorKind::Trap,
            }
        };
        Error::new(
            kind,
            format!("{}: {export}: {}\n{err}", self.name, err.root_cause()),
        )
    }
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
fn deliver_log(
    state: &mut State,
    ceiling: LogLevel,
    sink: Option<&LogSink>,
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
    state
        .log
        .charge(bytes)
        .map_err(|why| Error::new(ErrorKind::LimitExceeded, format!("{plugin}: {why}")))?;

    let Some(level) = LogLevel::from_wit_name(level) else {
        // A case this build does not know cannot be compared against the
        // ceiling, so it cannot be shown to satisfy it. Drop rather than
        // deliver: the manifest's ceiling has to hold across a revision of a
        // Phase 1 proposal that adds a case.
        return Ok(());
    };

    if level < ceiling {
        return Ok(());
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
