//! Many plugins, one engine.
//!
//! The engine holds the compiled-code cache and the JIT, and is the expensive
//! thing to build; a store is per-plugin and cheap. A registry keeps that split
//! visible: plugins share compilation, and share nothing else.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::path::Path;

use wasmtime::component::Val;

use crate::{Error, ErrorKind, Host, Plugin, Result};

/// A named collection of plugins loaded under one [`Host`].
#[derive(Debug)]
pub struct Registry {
    host: Host,
    plugins: BTreeMap<String, Plugin>,
}

impl Registry {
    /// Create an empty registry over a host.
    #[must_use]
    pub fn new(host: Host) -> Self {
        Self {
            host,
            plugins: BTreeMap::new(),
        }
    }

    /// The host every plugin here runs under.
    #[must_use]
    pub fn host(&self) -> &Host {
        &self.host
    }

    /// Load a component from disk, naming it after the file stem.
    ///
    /// Returns the name it was registered under.
    pub fn load(&mut self, path: impl AsRef<Path>) -> Result<String> {
        let plugin = self.host.load(path)?;
        let name = plugin.name().to_string();
        self.insert(plugin)?;
        Ok(name)
    }

    /// Load a component already in memory, under an explicit name.
    pub fn load_binary(&mut self, name: &str, wasm: &[u8]) -> Result<()> {
        let plugin = self.host.load_binary(name, wasm)?;
        self.insert(plugin)
    }

    fn insert(&mut self, plugin: Plugin) -> Result<()> {
        match self.plugins.entry(plugin.name().to_string()) {
            // Silently replacing would make a double-load look like it worked
            // while the first plugin's state vanished.
            Entry::Occupied(entry) => Err(Error::new(
                ErrorKind::InvalidArgument,
                format!("a plugin named {:?} is already registered", entry.key()),
            )),
            Entry::Vacant(entry) => {
                entry.insert(plugin);
                Ok(())
            }
        }
    }

    /// Names of every registered plugin, in order.
    pub fn names(&self) -> impl ExactSizeIterator<Item = &str> {
        self.plugins.keys().map(String::as_str)
    }

    /// How many plugins are registered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.plugins.len()
    }

    /// Whether no plugins are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }

    /// Borrow one plugin.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Plugin> {
        self.plugins.get(name)
    }

    /// Borrow one plugin mutably. Calling into a plugin needs `&mut`, because
    /// a call mutates its store.
    #[must_use]
    pub fn get_mut(&mut self, name: &str) -> Option<&mut Plugin> {
        self.plugins.get_mut(name)
    }

    /// Unload a plugin, dropping its store and everything in it.
    pub fn remove(&mut self, name: &str) -> Option<Plugin> {
        self.plugins.remove(name)
    }

    /// Call an export on a named plugin.
    pub fn call(&mut self, plugin: &str, export: &str, args: &[Val]) -> Result<Vec<Val>> {
        self.get_mut(plugin)
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::NotFound,
                    format!("no plugin named {plugin:?} is registered"),
                )
            })?
            .call(export, args)
    }
}
