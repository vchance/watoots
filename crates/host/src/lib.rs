//! watoots — a sandboxed plugin host for native applications, on the
//! WebAssembly component model.
//!
//! An application declares a WIT world for its plugin interface and a manifest
//! saying what plugins may touch. The host compiles a component, intersects its
//! declared imports against those grants, and refuses to load anything the
//! manifest does not cover:
//!
//! ```no_run
//! use watoots::Host;
//!
//! let host = Host::builder()
//!     .manifest_from_file("plugins/policy.toml")?
//!     .build()?;
//!
//! let mut plugin = host.load("plugins/lint.wasm")?;
//! let out = plugin.call("name", &[])?;
//! # Ok::<(), watoots::Error>(())
//! ```
//!
//! The manifest is the product. Everything here exists to enforce it, and the
//! enforcement point is deliberately *load* time rather than call time: a
//! plugin that wants the network says so in its imports, and you find that out
//! when you install it.
//!
//! # Status
//!
//! M1. The engine, manifest, import-intersection check, and per-call limits
//! work. Not yet here: typed host APIs (so a component importing an
//! application's own interface passes [`Host::inspect`] but cannot instantiate),
//! the precompile cache, the registry, WAVE calls, and network-allowlist
//! enforcement.

#![warn(missing_docs)]

mod error;
pub mod fuzz;
mod host;
pub mod imports;
pub mod manifest;
mod plugin;
mod registry;
pub mod trace;
pub mod wave;

pub use error::{Error, ErrorKind, Result};
pub use host::{Host, HostBuilder, HostCall, HostFunc, ImportedFunction, LogRecord, LogSink};
pub use imports::{GrantReport, ImportDecision, Requirement};
pub use manifest::{Clocks, FsGrants, Limits, LogLevel, Manifest, Permissions};
pub use plugin::{Plugin, PluginStats};
pub use registry::Registry;
pub use trace::{Outcome, TraceEvent, TraceHook};
pub use wave::{from_wave, to_wave};

/// Re-exported so callers can build arguments without depending on wasmtime
/// directly.
pub use wasmtime::component::Val;

/// Re-exported for the same reason as [`Val`]: a caller working dynamically —
/// the CLI, the fuzzer, a host function answering in text — needs the world's
/// own declaration of a type before it can produce a value of it.
pub use wasmtime::component::Type;
