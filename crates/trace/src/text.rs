//! The text encoding: line-oriented, one value per line.
//!
//! Readability is the whole point of this format existing — Wasmtime's own
//! engine-level recorder is deliberately binary and unreadable, and this is
//! where we differ. So: one event per stanza, one value per line, no value
//! split across lines. A trace diffs cleanly, and a reviewer can edit one to
//! construct a case that never happened.

use crate::trace::{Event, FORMAT_VERSION, Header, Outcome, Trace};
use crate::{Error, Result};

const MAGIC: &str = "watoots-trace";

/// Render a trace as text.
#[must_use]
pub fn to_text(trace: &Trace) -> String {
    let mut out = String::new();
    out.push_str(&format!("{MAGIC} {FORMAT_VERSION}\n"));
    out.push_str(&format!(
        "component sha256:{}\n",
        trace.header.component_sha256
    ));
    out.push_str(&format!("plugin {}\n", trace.header.plugin));
    if !trace.header.manifest_toml.trim().is_empty() {
        out.push_str("manifest\n");
        for line in trace.header.manifest_toml.lines() {
            out.push_str("  ");
            out.push_str(line);
            out.push('\n');
        }
    }
    out.push('\n');

    for event in &trace.events {
        match event {
            Event::ExportCall { func, args } => {
                out.push_str(&format!("export-call {func}\n"));
                push_args(&mut out, args);
            }
            Event::ExportReturn { func, outcome } => {
                out.push_str(&format!("export-return {func}\n"));
                push_outcome(&mut out, outcome);
            }
            Event::ImportCall {
                interface,
                func,
                args,
            } => {
                out.push_str(&format!("import-call {interface} {func}\n"));
                push_args(&mut out, args);
            }
            Event::ImportReturn {
                interface,
                func,
                outcome,
            } => {
                out.push_str(&format!("import-return {interface} {func}\n"));
                push_outcome(&mut out, outcome);
            }
        }
    }
    out
}

fn push_args(out: &mut String, args: &[String]) {
    for arg in args {
        out.push_str(&format!("  arg {arg}\n"));
    }
}

fn push_outcome(out: &mut String, outcome: &Outcome) {
    match outcome {
        Outcome::Value(None) => out.push_str("  unit\n"),
        Outcome::Value(Some(value)) => out.push_str(&format!("  value {value}\n")),
        Outcome::Error { status, message } => {
            out.push_str(&format!("  error {status} {}\n", quote(message)));
        }
    }
}

/// Quote a string on one line. Escapes match WAVE's, so a message reads the
/// same way a recorded string value does.
fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

fn unquote(text: &str) -> Result<String> {
    let inner = text
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .ok_or_else(|| Error::new(format!("expected a quoted string, got {text:?}")))?;

    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(character) = chars.next() {
        if character != '\\' {
            out.push(character);
            continue;
        }
        match chars.next() {
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some(other) => return Err(Error::new(format!("unknown escape \\{other}"))),
            None => return Err(Error::new("string ends in a backslash")),
        }
    }
    Ok(out)
}

/// Parse a trace from text.
pub fn from_text(text: &str) -> Result<Trace> {
    let mut lines = text.lines().enumerate().peekable();
    let mut header = Header::default();

    // Header
    let (_, first) = lines
        .next()
        .ok_or_else(|| Error::new("empty trace: no format line"))?;
    let version = first
        .strip_prefix(MAGIC)
        .map(str::trim)
        .ok_or_else(|| Error::new(format!("not a watoots trace: {first:?}")))?;
    let version: u32 = version
        .parse()
        .map_err(|_| Error::new(format!("unreadable format version {version:?}")))?;
    if version != FORMAT_VERSION {
        return Err(Error::new(format!(
            "trace format version {version}, this build reads {FORMAT_VERSION}"
        )));
    }

    let mut events = Vec::new();

    while let Some((number, line)) = lines.next() {
        let at = number + 1;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Indented lines belong to the event above and are consumed there.
        let (keyword, rest) = split_once(trimmed);
        match keyword {
            "component" => {
                header.component_sha256 = rest
                    .strip_prefix("sha256:")
                    .ok_or_else(|| Error::new(format!("line {at}: expected sha256:<hex>")))?
                    .to_string();
            }
            "plugin" => header.plugin = rest.to_string(),
            "manifest" => {
                let mut manifest = String::new();
                while let Some((_, next)) = lines.peek() {
                    if !next.starts_with("  ") {
                        break;
                    }
                    manifest.push_str(&next[2..]);
                    manifest.push('\n');
                    lines.next();
                }
                header.manifest_toml = manifest;
            }
            "export-call" => {
                let args = collect_args(&mut lines);
                events.push(Event::ExportCall {
                    func: rest.to_string(),
                    args,
                });
            }
            "export-return" => {
                let outcome = collect_outcome(&mut lines, at)?;
                events.push(Event::ExportReturn {
                    func: rest.to_string(),
                    outcome,
                });
            }
            "import-call" => {
                let (interface, func) = split_pair(rest, at)?;
                let args = collect_args(&mut lines);
                events.push(Event::ImportCall {
                    interface,
                    func,
                    args,
                });
            }
            "import-return" => {
                let (interface, func) = split_pair(rest, at)?;
                let outcome = collect_outcome(&mut lines, at)?;
                events.push(Event::ImportReturn {
                    interface,
                    func,
                    outcome,
                });
            }
            other => {
                return Err(Error::new(format!("line {at}: unknown keyword {other:?}")));
            }
        }
    }

    Ok(Trace { header, events })
}

type Lines<'a> = std::iter::Peekable<std::iter::Enumerate<std::str::Lines<'a>>>;

fn split_once(line: &str) -> (&str, &str) {
    line.split_once(char::is_whitespace)
        .map_or((line, ""), |(head, tail)| (head, tail.trim_start()))
}

fn split_pair(rest: &str, at: usize) -> Result<(String, String)> {
    let (interface, func) = rest
        .split_once(char::is_whitespace)
        .ok_or_else(|| Error::new(format!("line {at}: expected <interface> <function>")))?;
    Ok((interface.to_string(), func.trim().to_string()))
}

fn collect_args(lines: &mut Lines<'_>) -> Vec<String> {
    let mut args = Vec::new();
    while let Some((_, next)) = lines.peek() {
        let Some(value) = next.trim().strip_prefix("arg ") else {
            break;
        };
        args.push(value.to_string());
        lines.next();
    }
    args
}

fn collect_outcome(lines: &mut Lines<'_>, at: usize) -> Result<Outcome> {
    let Some((_, next)) = lines.peek().copied() else {
        return Err(Error::new(format!("line {at}: missing outcome")));
    };
    let trimmed = next.trim();
    let (keyword, rest) = split_once(trimmed);
    let outcome = match keyword {
        "unit" => Outcome::Value(None),
        "value" => Outcome::Value(Some(rest.to_string())),
        "error" => {
            let (status, message) = rest
                .split_once(char::is_whitespace)
                .ok_or_else(|| Error::new(format!("line {at}: expected <status> <message>")))?;
            Outcome::Error {
                status: status.to_string(),
                message: unquote(message.trim())?,
            }
        }
        other => {
            return Err(Error::new(format!(
                "line {at}: expected unit/value/error, got {other:?}"
            )));
        }
    };
    lines.next();
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Trace {
        Trace {
            header: Header {
                component_sha256: "abc123".to_string(),
                plugin: "rust_lint".to_string(),
                manifest_toml: "[permissions]\nclocks = \"monotonic\"\n".to_string(),
            },
            events: vec![
                Event::ExportCall {
                    func: "lint".to_string(),
                    args: vec!["\"notes.md\"".to_string(), "\"TODO\\n\"".to_string()],
                },
                Event::ImportCall {
                    interface: "watoots:example/log@0.1.0".to_string(),
                    func: "emit".to_string(),
                    args: vec!["hint".to_string(), "\"linting notes.md\"".to_string()],
                },
                Event::ImportReturn {
                    interface: "watoots:example/log@0.1.0".to_string(),
                    func: "emit".to_string(),
                    outcome: Outcome::Value(None),
                },
                Event::ExportReturn {
                    func: "lint".to_string(),
                    outcome: Outcome::Value(Some("[{line: 1}]".to_string())),
                },
            ],
        }
    }

    #[test]
    fn round_trips_through_text() {
        let trace = sample();
        let parsed = from_text(&to_text(&trace)).unwrap();
        assert_eq!(parsed, trace);
    }

    #[test]
    fn round_trips_an_error_outcome() {
        let mut trace = sample();
        trace.events.push(Event::ExportReturn {
            func: "lint".to_string(),
            outcome: Outcome::Error {
                status: "WT_ERR_TRAP".to_string(),
                // Newlines and quotes must survive one line of text.
                message: "trap:\n  \"unreachable\"".to_string(),
            },
        });
        assert_eq!(from_text(&to_text(&trace)).unwrap(), trace);
    }

    #[test]
    fn the_rendering_is_readable() {
        let text = to_text(&sample());
        assert!(text.starts_with("watoots-trace 1\n"), "{text}");
        assert!(
            text.contains("export-call lint\n  arg \"notes.md\""),
            "{text}"
        );
        assert!(
            text.contains("import-call watoots:example/log@0.1.0 emit"),
            "{text}"
        );
        // One value per line: that is what makes a trace diff cleanly.
        for line in text.lines() {
            assert!(!line.contains("\r"), "{line}");
        }
    }

    #[test]
    fn rejects_a_file_that_is_not_a_trace() {
        assert!(from_text("hello\n").is_err());
    }

    #[test]
    fn rejects_a_future_format_version() {
        let err = from_text("watoots-trace 99\n").unwrap_err();
        assert!(err.message().contains("99"), "{}", err.message());
    }

    #[test]
    fn rejects_an_unknown_keyword() {
        let err = from_text("watoots-trace 1\nwiggle 3\n").unwrap_err();
        assert!(err.message().contains("wiggle"), "{}", err.message());
    }
}
