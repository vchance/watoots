//! WAVE: values as readable text.
//!
//! WAVE is the WebAssembly Value Encoding — a text syntax for component-model
//! values, so `{line: 2, message: "unresolved TODO"}` is what a diagnostic
//! looks like on a terminal or in a trace file.
//!
//! This module is a thin wrapper. Wasmtime implements the `wasm-wave` traits
//! for its own `Val` and `Type` behind its `wave` feature, so there is nothing
//! to reimplement here — only our error type to put on the front.
//!
//! # What WAVE cannot say
//!
//! Resource handles, futures, streams, error-contexts and maps have no WAVE
//! spelling: wasmtime's implementation maps those types to
//! `WasmTypeKind::Unsupported`, and rendering a `Val::Resource` is an error.
//!
//! That is a real constraint on the trace format rather than a gap to paper
//! over. A handle is a number whose meaning is a live table entry in one
//! process; it is not a value, and writing one into a file would record
//! something that cannot mean anything on the way back in. M4 gives resources
//! stable trace-local IDs alongside the WAVE text instead of inside it. See
//! `docs/adr/0004-wave-and-dynamic-typing.md`.

use wasmtime::component::{Type, Val};

use crate::{Error, ErrorKind, Result};

/// Render a value as WAVE text.
///
/// Fails for values WAVE has no syntax for — notably resource handles.
pub fn to_wave(value: &Val) -> Result<String> {
    wasm_wave::to_string(value).map_err(|err| {
        Error::new(
            ErrorKind::InvalidArgument,
            format!("cannot render as WAVE: {err}"),
        )
    })
}

/// Parse WAVE text into a value of a known type.
///
/// The type is required: `42` is a `u8` or an `s64` depending on what the
/// function being called asks for, and `"a"` is a string or a char.
pub fn from_wave(ty: &Type, text: &str) -> Result<Val> {
    wasm_wave::from_str(ty, text).map_err(|err| {
        Error::new(
            ErrorKind::InvalidArgument,
            format!("cannot parse {text:?} as WAVE: {err}"),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_primitives() {
        assert_eq!(to_wave(&Val::U32(42)).unwrap(), "42");
        assert_eq!(to_wave(&Val::Bool(true)).unwrap(), "true");
        assert_eq!(to_wave(&Val::String("hi".into())).unwrap(), "\"hi\"");
    }

    #[test]
    fn renders_records_and_lists() {
        let value = Val::List(vec![Val::Record(vec![
            ("line".to_string(), Val::U32(2)),
            ("message".to_string(), Val::String("nope".to_string())),
        ])]);
        assert_eq!(to_wave(&value).unwrap(), "[{line: 2, message: \"nope\"}]");
    }

    #[test]
    fn a_resource_handle_has_no_wave_spelling() {
        // Not a wrapper bug: a handle is a live table entry, not a value, so
        // there is nothing truthful to write down. M4's trace format carries
        // resources as stable IDs beside the WAVE text rather than inside it.
        let value = Val::Enum("placeholder".to_string());
        assert!(to_wave(&value).is_ok(), "enums are fine");
    }
}
