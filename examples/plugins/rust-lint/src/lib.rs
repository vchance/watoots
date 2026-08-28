//! A sample watoots plugin: a tiny linter, in Rust.
//!
//! Build it with:
//!
//! ```sh
//! cargo build --manifest-path examples/plugins/rust-lint/Cargo.toml \
//!     --target wasm32-wasip2 --release
//! ```
//!
//! The point is not the lint rules. It is that this file imports a host
//! function and exports typed functions over the WIT world in `examples/wit`,
//! and the host serves the one and calls the other without either side
//! agreeing on a serialization format.

wit_bindgen::generate!({
    path: "../../wit",
    world: "lint-plugin",
});

use crate::watoots::example::log;
use crate::watoots::example::types::Severity;

// `Diagnostic` arrives at the root already, via `use types.{diagnostic}` in the
// world; importing it again from the interface would be a duplicate.

struct RustLint;

impl Guest for RustLint {
    fn name() -> String {
        "rust-lint".to_string()
    }

    fn lint(path: String, source: String) -> Vec<Diagnostic> {
        // Calling into the host: this is an import crossing, and the one M4's
        // recorder will capture.
        log::emit(Severity::Hint, &format!("linting {path}"));

        let mut diagnostics = Vec::new();

        for (index, line) in source.lines().enumerate() {
            let line_number = (index + 1) as u32;

            if line.len() > 80 {
                diagnostics.push(Diagnostic {
                    line: line_number,
                    column: 81,
                    severity: Severity::Warning,
                    message: format!("line is {} characters, over 80", line.len()),
                });
            }

            if line.ends_with(' ') || line.ends_with('\t') {
                diagnostics.push(Diagnostic {
                    line: line_number,
                    column: line.len() as u32,
                    severity: Severity::Hint,
                    message: "trailing whitespace".to_string(),
                });
            }

            if line.contains("TODO") {
                diagnostics.push(Diagnostic {
                    line: line_number,
                    column: (line.find("TODO").unwrap_or(0) + 1) as u32,
                    severity: Severity::Error,
                    message: "unresolved TODO".to_string(),
                });
            }
        }

        log::emit(
            Severity::Hint,
            &format!("{} diagnostic(s)", diagnostics.len()),
        );

        diagnostics
    }
}

export!(RustLint);
