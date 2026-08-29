//! The binary encoding: the same events, length-prefixed.
//!
//! This is a *framing*, not a compression scheme. Values stay as WAVE text so
//! the two encodings are losslessly interconvertible and `watoots trace fmt`
//! can round-trip either way without a type table. What the binary form buys is
//! that reading it needs no parser, which matters when a trace is large or is
//! being read by something that is not Rust.

use crate::trace::{Event, FORMAT_VERSION, Header, Outcome, Trace};
use crate::{Error, Result};

const MAGIC: &[u8; 4] = b"WTTR";

/// Encode a trace.
#[must_use]
pub fn to_bytes(trace: &Trace) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    put_str(&mut out, &trace.header.component_sha256);
    put_str(&mut out, &trace.header.plugin);
    put_str(&mut out, &trace.header.manifest_toml);

    let count = u32::try_from(trace.events.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&count.to_le_bytes());

    for event in &trace.events {
        match event {
            Event::ExportCall { func, args } => {
                out.push(0);
                put_str(&mut out, func);
                put_strs(&mut out, args);
            }
            Event::ExportReturn { func, outcome } => {
                out.push(1);
                put_str(&mut out, func);
                put_outcome(&mut out, outcome);
            }
            Event::ImportCall {
                interface,
                func,
                args,
            } => {
                out.push(2);
                put_str(&mut out, interface);
                put_str(&mut out, func);
                put_strs(&mut out, args);
            }
            Event::ImportReturn {
                interface,
                func,
                outcome,
            } => {
                out.push(3);
                put_str(&mut out, interface);
                put_str(&mut out, func);
                put_outcome(&mut out, outcome);
            }
        }
    }
    out
}

fn put_str(out: &mut Vec<u8>, text: &str) {
    let len = u32::try_from(text.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&text.as_bytes()[..len as usize]);
}

fn put_strs(out: &mut Vec<u8>, values: &[String]) {
    let count = u32::try_from(values.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&count.to_le_bytes());
    for value in values {
        put_str(out, value);
    }
}

fn put_outcome(out: &mut Vec<u8>, outcome: &Outcome) {
    match outcome {
        Outcome::Value(None) => out.push(0),
        Outcome::Value(Some(value)) => {
            out.push(1);
            put_str(out, value);
        }
        Outcome::Error { status, message } => {
            out.push(2);
            put_str(out, status);
            put_str(out, message);
        }
    }
}

/// A cursor that refuses to read past the end rather than panicking, because
/// the input is a file that may be truncated or hostile.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8]> {
        let end = self
            .at
            .checked_add(count)
            .filter(|end| *end <= self.bytes.len())
            .ok_or_else(|| {
                Error::new(format!(
                    "trace ends early: wanted {count} bytes at offset {}",
                    self.at
                ))
            })?;
        let slice = &self.bytes[self.at..end];
        self.at = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes(bytes.try_into().expect("4 bytes")))
    }

    fn string(&mut self) -> Result<String> {
        let len = self.u32()? as usize;
        let bytes = self.take(len)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| Error::new("trace contains invalid UTF-8"))
    }

    fn strings(&mut self) -> Result<Vec<String>> {
        let count = self.u32()? as usize;
        (0..count).map(|_| self.string()).collect()
    }

    fn outcome(&mut self) -> Result<Outcome> {
        match self.u8()? {
            0 => Ok(Outcome::Value(None)),
            1 => Ok(Outcome::Value(Some(self.string()?))),
            2 => Ok(Outcome::Error {
                status: self.string()?,
                message: self.string()?,
            }),
            other => Err(Error::new(format!("unknown outcome tag {other}"))),
        }
    }
}

/// Decode a trace.
pub fn from_bytes(bytes: &[u8]) -> Result<Trace> {
    let mut reader = Reader { bytes, at: 0 };
    if reader.take(4)? != MAGIC {
        return Err(Error::new("not a watoots binary trace"));
    }
    let version = reader.u32()?;
    if version != FORMAT_VERSION {
        return Err(Error::new(format!(
            "trace format version {version}, this build reads {FORMAT_VERSION}"
        )));
    }

    let header = Header {
        component_sha256: reader.string()?,
        plugin: reader.string()?,
        manifest_toml: reader.string()?,
    };

    let count = reader.u32()? as usize;
    let mut events = Vec::with_capacity(count.min(4096));
    for _ in 0..count {
        events.push(match reader.u8()? {
            0 => Event::ExportCall {
                func: reader.string()?,
                args: reader.strings()?,
            },
            1 => Event::ExportReturn {
                func: reader.string()?,
                outcome: reader.outcome()?,
            },
            2 => Event::ImportCall {
                interface: reader.string()?,
                func: reader.string()?,
                args: reader.strings()?,
            },
            3 => Event::ImportReturn {
                interface: reader.string()?,
                func: reader.string()?,
                outcome: reader.outcome()?,
            },
            other => return Err(Error::new(format!("unknown event tag {other}"))),
        });
    }

    Ok(Trace { header, events })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Trace {
        Trace {
            header: Header {
                component_sha256: "abc123".to_string(),
                plugin: "rust_lint".to_string(),
                manifest_toml: "[permissions]\nrandom = true\n".to_string(),
            },
            events: vec![
                Event::ExportCall {
                    func: "lint".to_string(),
                    args: vec!["\"a.md\"".to_string()],
                },
                Event::ImportCall {
                    interface: "watoots:example/log@0.1.0".to_string(),
                    func: "emit".to_string(),
                    args: vec!["hint".to_string()],
                },
                Event::ImportReturn {
                    interface: "watoots:example/log@0.1.0".to_string(),
                    func: "emit".to_string(),
                    outcome: Outcome::Value(None),
                },
                Event::ExportReturn {
                    func: "lint".to_string(),
                    outcome: Outcome::Error {
                        status: "WT_ERR_TRAP".to_string(),
                        message: "boom".to_string(),
                    },
                },
            ],
        }
    }

    #[test]
    fn round_trips_through_binary() {
        let trace = sample();
        assert_eq!(from_bytes(&to_bytes(&trace)).unwrap(), trace);
    }

    #[test]
    fn the_two_encodings_agree() {
        // `watoots trace fmt` converts between them, so they must describe
        // exactly the same trace or the conversion loses something.
        let trace = sample();
        let via_text = crate::text::from_text(&crate::text::to_text(&trace)).unwrap();
        let via_binary = from_bytes(&to_bytes(&trace)).unwrap();
        assert_eq!(via_text, via_binary);
    }

    #[test]
    fn rejects_a_file_that_is_not_a_trace() {
        assert!(from_bytes(b"nope").is_err());
    }

    #[test]
    fn a_truncated_trace_is_an_error_not_a_panic() {
        // The input is a file, which may be truncated or hostile.
        let bytes = to_bytes(&sample());
        for cut in [4, 8, 20, bytes.len() / 2, bytes.len() - 1] {
            assert!(from_bytes(&bytes[..cut]).is_err(), "cut at {cut}");
        }
    }
}
