//! The engine, the policy, and loading plugins under it.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use wasmtime::component::Component;
use wasmtime::{Config, Engine};

use crate::imports::{self, GrantReport};
use crate::manifest::Manifest;
use crate::plugin::Plugin;
use crate::{Error, ErrorKind, Result};

/// How often the epoch ticker advances the engine's epoch.
///
/// Deadlines are expressed in whole ticks, so this is also the granularity of a
/// `timeout`: a 200ms timeout is 200 ticks.
const EPOCH_TICK: Duration = Duration::from_millis(1);

/// A configured engine plus the policy every plugin loaded from it runs under.
///
/// Cloning a `Host` shares the engine, the compiled-code cache that comes with
/// it, and the epoch ticker.
#[derive(Clone)]
pub struct Host {
    inner: Arc<HostInner>,
}

struct HostInner {
    engine: Engine,
    manifest: Manifest,
    host_provided: BTreeSet<String>,
    vars: BTreeMap<String, String>,
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

        Plugin::instantiate(name, &self.inner.engine, &component, &manifest, report)
    }

    fn compile(&self, wasm: &[u8]) -> Result<Component> {
        Component::new(&self.inner.engine, wasm)
            .map_err(|err| Error::new(ErrorKind::Load, format!("{err:?}")))
    }

    fn report_for(&self, component: &Component) -> GrantReport {
        let component_type = component.component_type();
        let names: Vec<&str> = component_type
            .imports(&self.inner.engine)
            .map(|(name, _)| name)
            .collect();
        imports::check(
            names,
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
            .field("vars", &self.inner.vars)
            .finish_non_exhaustive()
    }
}

/// Builder for [`Host`].
#[derive(Debug, Default)]
pub struct HostBuilder {
    manifest: Manifest,
    host_provided: BTreeSet<String>,
    vars: BTreeMap<String, String>,
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

    /// Declare that the application itself serves this interface, by
    /// unversioned name (`watoots:example/log`).
    ///
    /// Declaring an interface only settles the *permission* question. Actually
    /// serving its functions to the guest arrives with typed host APIs in M2,
    /// so a component importing one will pass [`Host::inspect`] and fail to
    /// instantiate until then.
    #[must_use]
    pub fn provide_interface(mut self, interface: impl Into<String>) -> Self {
        self.host_provided.insert(interface.into());
        self
    }

    /// Define a `${name}` substitution for manifest paths.
    #[must_use]
    pub fn var(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.vars.insert(name.into(), value.into());
        self
    }

    /// Build the host and its engine.
    pub fn build(self) -> Result<Host> {
        let limits = &self.manifest.limits;

        // Network grants parse and take part in the import check, but nothing
        // enforces the allowlist itself yet. Refusing here keeps default-deny
        // honest: the alternative is handing a plugin the whole network because
        // the manifest named one host.
        if !self.manifest.permissions.net.is_empty() {
            return Err(Error::new(
                ErrorKind::Manifest,
                "permissions.net is parsed but not yet enforced (M2); \
                 remove the grant rather than run with the network open",
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
                vars: self.vars,
                _ticker: ticker,
            }),
        })
    }
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
