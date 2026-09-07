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
//! Pre-1.0: the API can still move. The engine, manifest, import-intersection
//! check, per-call limits, precompile cache, registry, WAVE calls, reload,
//! profiling and the trace and audit hooks are all here.
//!
//! What is deliberately not here: a `permissions.net` host allowlist, which
//! cannot be enforced at this layer (see [`NetGrant`] and ADR-0012);
//! same-binary checkpoint/restore, which Wasmtime 48 does not expose the state
//! for (ADR-0010); and recording of resource handles, which cannot be replayed
//! meaningfully.

#![warn(missing_docs)]

pub mod audit;
mod error;
pub mod fuzz;
mod host;
pub mod imports;
pub mod manifest;
mod plugin;
mod profile;
mod registry;
pub mod signature;
pub mod trace;
pub mod wave;

pub use audit::{AuditEvent, AuditHook, Ceiling, Verdict};
pub use error::{Error, ErrorKind, Result};
pub use host::{Host, HostBuilder, HostCall, HostFunc, ImportedFunction, LogRecord, LogSink};
pub use imports::{GrantReport, ImportDecision, Requirement};
pub use manifest::{
    Clocks, DEFAULT_TRANSFER_BYTES, FsGrants, Limits, LogLevel, Manifest, NetGrant, Permissions,
};
pub use plugin::{Plugin, PluginStats, RESTORE_STATE_EXPORT, ReloadReport, SAVE_STATE_EXPORT};
pub use profile::{FunctionKind, FunctionProfile, PluginProfile, Profiling};
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
