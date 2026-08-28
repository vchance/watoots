//! A loaded, instantiated component and the store it runs in.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use wasmtime::component::{Component, Linker, ResourceTable, Val};
use wasmtime::{Engine, Store, StoreLimits, StoreLimitsBuilder};
use wasmtime_wasi::{FsPerms, WasiCtx, WasiCtxView, WasiView};

use crate::host::HostFunc;
use crate::imports::GrantReport;
use crate::manifest::{Limits, Manifest};
use crate::trace::{Outcome, TraceEvent, TraceHook};
use crate::{Error, ErrorKind, Result};

/// Everything the guest can reach, plus the ceilings it runs under.
struct State {
    wasi: WasiCtx,
    table: ResourceTable,
    limits: StoreLimits,
}

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

        let state = State {
            wasi,
            table: ResourceTable::new(),
            limits: StoreLimitsBuilder::new().memory_size(memory).build(),
        };

        let mut store = Store::new(engine, state);
        store.limiter(|state| &mut state.limits);

        let mut linker: Linker<State> = Linker::new(engine);
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker)
            .map_err(|err| Error::new(ErrorKind::Internal, format!("wiring WASI: {err:?}")))?;

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

        Ok(Self {
            name: name.to_string(),
            store,
            instance,
            limits: manifest.limits.clone(),
            report,
            trace: wiring.trace.map(Arc::clone),
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
        let kind = match err.downcast_ref::<wasmtime::Trap>() {
            Some(wasmtime::Trap::OutOfFuel | wasmtime::Trap::Interrupt) => ErrorKind::LimitExceeded,
            _ => ErrorKind::Trap,
        };
        Error::new(
            kind,
            format!("{}: {export}: {}\n{err}", self.name, err.root_cause()),
        )
    }
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
                .func_new(func_name, move |_store, ty, params, results| {
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
    if let Some(fuel) = limits.fuel {
        store.set_fuel(fuel).map_err(|err| {
            Error::new(
                ErrorKind::Internal,
                format!("{name}: cannot set fuel: {err:?}"),
            )
        })?;
    }
    if let Some(timeout) = limits.timeout {
        // Deadlines are counted in epoch ticks; see EPOCH_TICK in host.rs.
        let ticks = u64::try_from(timeout.as_millis())
            .unwrap_or(u64::MAX)
            .max(1);
        store.set_epoch_deadline(ticks);
    }
    Ok(())
}

/// Turn manifest grants into a WASI context.
///
/// Only the filesystem and environment need building here. Clocks, randomness,
/// and sockets are denied by *not being importable*: the intersection check
/// refuses to load a component that imports an interface the manifest does not
/// grant, so there is nothing to switch off in the context afterwards.
fn build_wasi_ctx(manifest: &Manifest) -> Result<WasiCtx> {
    let permissions = &manifest.permissions;
    let mut builder = WasiCtx::builder();

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

fn preopen(builder: &mut wasmtime_wasi::WasiCtxBuilder, path: &str, perms: FsPerms) -> Result<()> {
    builder.preopened_dir(path, path, perms).map_err(|err| {
        Error::new(
            ErrorKind::Manifest,
            format!("cannot preopen {path:?} granted by the manifest: {err}"),
        )
    })?;
    Ok(())
}
