//! The trace data model.

use std::fmt;

/// The format version this build reads and writes.
pub const FORMAT_VERSION: u32 = 1;

/// What a call produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The call returned. `None` for a function that returns nothing.
    ///
    /// The value is WAVE text, which is what makes a trace diffable: a
    /// reviewer reads `{line: 2, message: "unresolved TODO"}` rather than a
    /// hex dump, and can edit it to construct a case by hand.
    Value(Option<String>),
    /// The call failed.
    Error {
        /// The `wt_status` spelling, e.g. `WT_ERR_TRAP`.
        status: String,
        /// The message.
        message: String,
    },
}

/// One crossing of the host/plugin boundary.
///
/// Exports go in, imports come back out, and the order is the trace. Replay
/// drives the exports and answers the imports from exactly this sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The application called into the plugin.
    ExportCall {
        /// Exported function name.
        func: String,
        /// Arguments as WAVE text.
        args: Vec<String>,
    },
    /// A plugin export returned.
    ExportReturn {
        /// Exported function name.
        func: String,
        /// What came back.
        outcome: Outcome,
    },
    /// The plugin called out to a host function.
    ImportCall {
        /// Interface, version included.
        interface: String,
        /// Function name within the interface.
        func: String,
        /// Arguments as WAVE text.
        args: Vec<String>,
    },
    /// A host function answered.
    ImportReturn {
        /// Interface, version included.
        interface: String,
        /// Function name within the interface.
        func: String,
        /// What the host answered.
        outcome: Outcome,
    },
}

impl Event {
    /// A short one-line description, for divergence reports.
    #[must_use]
    pub fn summary(&self) -> String {
        match self {
            Self::ExportCall { func, args } => format!("call {func}({})", args.join(", ")),
            Self::ExportReturn { func, .. } => format!("return from {func}"),
            Self::ImportCall {
                interface,
                func,
                args,
            } => format!("{interface}#{func}({})", args.join(", ")),
            Self::ImportReturn {
                interface, func, ..
            } => format!("return from {interface}#{func}"),
        }
    }
}

impl fmt::Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.summary())
    }
}

/// What a trace was recorded against.
///
/// Enough to refuse a replay against the wrong component, and enough to rebuild
/// the host: the manifest travels with the trace so a replay needs only the
/// trace file and the `.wasm`, never the application that produced it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Header {
    /// Hex SHA-256 of the component bytes.
    pub component_sha256: String,
    /// The name the plugin was loaded under.
    pub plugin: String,
    /// The manifest in force when the trace was recorded, as TOML.
    pub manifest_toml: String,
}

/// A recorded session: a header and an ordered list of crossings.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Trace {
    /// What this was recorded against.
    pub header: Header,
    /// Every crossing, in order.
    pub events: Vec<Event>,
}

impl Trace {
    /// Hex SHA-256 of some component bytes, in the spelling the header uses.
    #[must_use]
    pub fn hash_component(wasm: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(wasm);
        digest.iter().fold(String::new(), |mut out, byte| {
            use fmt::Write as _;
            let _ = write!(out, "{byte:02x}");
            out
        })
    }

    /// Every distinct `(interface, function)` the plugin called out to.
    ///
    /// Replay uses this to know which host functions its mock has to serve.
    #[must_use]
    pub fn imported_functions(&self) -> Vec<(String, String)> {
        let mut seen: Vec<(String, String)> = Vec::new();
        for event in &self.events {
            if let Event::ImportCall {
                interface, func, ..
            } = event
            {
                let pair = (interface.clone(), func.clone());
                if !seen.contains(&pair) {
                    seen.push(pair);
                }
            }
        }
        seen
    }
}
