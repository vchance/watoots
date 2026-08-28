//! The observation seam that record/replay is built on.
//!
//! Every crossing between the application and a plugin already funnels through
//! this crate: exports go out through [`Plugin::call`](crate::Plugin::call),
//! imports come back in through the host functions installed on the linker.
//! A [`TraceHook`] sees both.
//!
//! M4 implements a hook that serializes each event to a WIT-typed trace. This
//! module deliberately stops short of that: it defines *where* a recorder
//! attaches, not what a trace looks like, so the format can be designed against
//! a seam that already exists and is already exercised.

use wasmtime::component::Val;

use crate::Error;

/// How a crossing ended.
///
/// Deliberately *not* `#[non_exhaustive]`: see [`TraceEvent`].
#[derive(Debug)]
pub enum Outcome<'a> {
    /// The call returned these values.
    Returned(&'a [Val]),
    /// The call failed.
    Failed(&'a Error),
}

/// One crossing of the host/plugin boundary.
///
/// Exports are calls the application makes *into* a plugin; imports are calls
/// the plugin makes back *out*. Replay needs both: it drives the exports and
/// answers the imports.
///
/// Deliberately *not* `#[non_exhaustive]`. Marking it so would let us add a
/// crossing later without a major version bump, at the cost of forcing every
/// implementor to write a `_ => {}` arm — and for a recorder, that arm means
/// silently dropping events and writing an incomplete trace. A compile error is
/// the better failure: adding a variant should make every recorder say so.
#[derive(Debug)]
pub enum TraceEvent<'a> {
    /// The application is about to call a plugin export.
    ExportCall {
        /// Plugin the call is going to.
        plugin: &'a str,
        /// Exported function name.
        func: &'a str,
        /// Arguments, as the guest will see them.
        args: &'a [Val],
    },
    /// A plugin export returned or failed.
    ExportReturn {
        /// Plugin the call went to.
        plugin: &'a str,
        /// Exported function name.
        func: &'a str,
        /// What came back.
        outcome: Outcome<'a>,
    },
    /// A plugin is calling a host function.
    ImportCall {
        /// Plugin making the call.
        plugin: &'a str,
        /// Interface the function belongs to.
        interface: &'a str,
        /// Function name within the interface.
        func: &'a str,
        /// Arguments the guest passed.
        args: &'a [Val],
    },
    /// A host function returned or failed.
    ImportReturn {
        /// Plugin that made the call.
        plugin: &'a str,
        /// Interface the function belongs to.
        interface: &'a str,
        /// Function name within the interface.
        func: &'a str,
        /// What the host answered.
        outcome: Outcome<'a>,
    },
}

impl TraceEvent<'_> {
    /// The plugin this event belongs to.
    #[must_use]
    pub fn plugin(&self) -> &str {
        match self {
            Self::ExportCall { plugin, .. }
            | Self::ExportReturn { plugin, .. }
            | Self::ImportCall { plugin, .. }
            | Self::ImportReturn { plugin, .. } => plugin,
        }
    }
}

/// Observes every crossing of the host/plugin boundary.
///
/// A hook must not panic and should be cheap: it runs inline on every call, so
/// the cost is paid per crossing whether or not anyone is recording.
pub trait TraceHook: Send + Sync {
    /// Called once per crossing, before the call for `*Call` and after it for
    /// `*Return`.
    fn on_event(&self, event: &TraceEvent<'_>);
}
