//! WIT-level record and replay for watoots plugins.
//!
//! Every call between an application and a plugin already passes through the
//! host library. A [`Recorder`] writes each crossing down as a typed event; a
//! replay instantiates the same component under a *mock host* that answers its
//! imports from the recording and reports the first place the two diverge.
//!
//! ```text
//! RECORD   application ⇄ [recording hook] ⇄ component   → trace
//! REPLAY   (no application)  [mock host]  ⇄ component   → pass, or first divergence
//! ```
//!
//! The application never has to be present to reproduce a plugin bug, which
//! means a bug report can be a file and that file can become a regression test.
//!
//! # Why this is not Wasmtime's `rr`
//!
//! Wasmtime records at the canonical-ABI level for bit-exact engine
//! determinism, in a binary format that is explicitly not meant to be read.
//! This records at the WIT level: values are [WAVE] text, so a trace diffs
//! cleanly in review and can be edited by hand to construct a case that never
//! actually happened. The two compose rather than compete.
//!
//! # Limits in this version
//!
//! Resource handles have no WAVE spelling — a handle is an index into a live
//! table, not a value — so recording a world that passes resources across the
//! boundary fails loudly rather than writing down a number that cannot mean
//! anything on the way back in. See `docs/adr/0004-wave-and-dynamic-typing.md`.
//! Host-to-guest reentrancy and async/stream imports are likewise out of scope
//! for v0.1 and documented rather than half-supported.
//!
//! [WAVE]: https://github.com/bytecodealliance/wasm-tools

#![warn(missing_docs)]

pub mod binary;
mod error;
mod recorder;
mod replay;
pub mod text;
mod trace;

pub use error::{Error, Result};
pub use recorder::Recorder;
pub use replay::{Divergence, ReplayReport, replay};
pub use trace::{Event, FORMAT_VERSION, Header, Outcome, Trace};
