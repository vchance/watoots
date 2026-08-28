//! A loaded, instantiated component and the store it runs in.

use std::fmt;

use wasmtime::component::{Component, Linker, ResourceTable, Val};
use wasmtime::{Engine, Store, StoreLimits, StoreLimitsBuilder};
use wasmtime_wasi::{FsPerms, WasiCtx, WasiCtxView, WasiView};

use crate::imports::GrantReport;
use crate::manifest::{Limits, Manifest};
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
        manifest: &Manifest,
        report: GrantReport,
    ) -> Result<Self> {
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
    /// one the recorder will sit on in M4. Rust hosts with a static world get
    /// `bindgen!` instead, in M2.
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

        // Wasmtime 48 no longer needs an explicit post_return.
        func.call(&mut self.store, args, &mut results)
            .map_err(|err| self.classify_call_error(export, &err))?;

        Ok(results)
    }

    /// Separate "the plugin misbehaved" from "the plugin hit a ceiling", since
    /// the two mean different things to whoever installed it.
    fn classify_call_error(&self, export: &str, err: &wasmtime::Error) -> Error {
        let kind = match err.downcast_ref::<wasmtime::Trap>() {
            Some(wasmtime::Trap::OutOfFuel | wasmtime::Trap::Interrupt) => ErrorKind::LimitExceeded,
            Some(_) => ErrorKind::Trap,
            // Memory growth past StoreLimits surfaces as a plain error rather
            // than a trap, so match on what it says.
            None if format!("{err:?}").contains("exceeds the limit") => ErrorKind::LimitExceeded,
            None => ErrorKind::Trap,
        };
        Error::new(kind, format!("{}: {export}: {err:?}", self.name))
    }
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

    if !permissions.env.is_empty() {
        let pairs: Vec<(&str, &str)> = permissions
            .env
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
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
