//! Replay: run the component again with the application replaced by the trace.

use std::sync::{Arc, Mutex};

use watoots::{Error as HostError, ErrorKind, Host, Manifest};

use crate::trace::{Event, Outcome, Trace};
use crate::{Error, Result, UNREPRESENTABLE};

/// One place where replay did not match the recording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Divergence {
    /// Position in the trace's event list.
    pub index: usize,
    /// What the trace said should happen.
    pub expected: String,
    /// What happened instead.
    pub actual: String,
}

impl std::fmt::Display for Divergence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "event {}:\n  expected: {}\n  actual:   {}",
            self.index, self.expected, self.actual
        )
    }
}

/// What a replay found.
#[derive(Debug, Clone, Default)]
pub struct ReplayReport {
    /// Crossings that matched the recording.
    pub matched: usize,
    /// Where the two parted company, in order. Empty means the replay passed.
    pub divergences: Vec<Divergence>,
}

impl ReplayReport {
    /// Whether the replay reproduced the recording exactly.
    #[must_use]
    pub fn is_faithful(&self) -> bool {
        self.divergences.is_empty()
    }

    /// A human-readable summary.
    #[must_use]
    pub fn describe(&self) -> String {
        if self.is_faithful() {
            return format!("replay matched the trace ({} crossings)", self.matched);
        }
        let mut out = format!(
            "replay diverged after {} matching crossing(s):\n",
            self.matched
        );
        // The first divergence is the one worth reading: everything after it is
        // downstream of a guest that has already been told something different.
        for divergence in self.divergences.iter().take(1) {
            out.push_str(&divergence.to_string());
            out.push('\n');
        }
        if self.divergences.len() > 1 {
            out.push_str(&format!(
                "({} further divergence(s) suppressed)\n",
                self.divergences.len() - 1
            ));
        }
        out
    }
}

/// Walks the recorded events, matching what actually happens against them.
#[derive(Debug)]
struct Cursor {
    events: Vec<Event>,
    position: usize,
    report: ReplayReport,
}

impl Cursor {
    fn diverge(&mut self, expected: String, actual: String) {
        self.report.divergences.push(Divergence {
            index: self.position,
            expected,
            actual,
        });
    }

    /// Step over recorded crossings that nothing here answers.
    ///
    /// The mock host stands in for the *application's* interfaces. WASI imports
    /// are served by the host library, from the manifest that travels in the
    /// trace header — `wasi:logging` being the one that actually shows up in a
    /// recording, since a granted plugin's log calls are host-import crossings
    /// like any other. Those events are the point of a recorded bug report and
    /// are kept, but no mock consumes them, so leaving them under the cursor
    /// would report a divergence at the next import the application does serve.
    fn skip_unserved(&mut self) {
        loop {
            let unserved = matches!(
                self.events.get(self.position),
                Some(Event::ImportCall { interface, .. }) if interface.starts_with("wasi:")
            );
            if !unserved {
                return;
            }
            self.position += 1;
            if matches!(
                self.events.get(self.position),
                Some(Event::ImportReturn { .. })
            ) {
                self.position += 1;
            }
        }
    }

    /// The next import the trace expects, and the answer it recorded.
    ///
    /// Nothing is formatted or cloned on the matching path: this runs on every
    /// host call the guest makes, and a replay of a pixel pipeline makes many.
    fn expect_import(&mut self, interface: &str, func: &str, args: &[String]) -> Option<Outcome> {
        let actual = || format!("{interface}#{func}({})", args.join(", "));
        self.skip_unserved();

        let expected = match self.events.get(self.position) {
            None => {
                self.diverge("the trace to be over".to_string(), actual());
                return None;
            }
            Some(Event::ImportCall {
                interface: expected_interface,
                func: expected_func,
                args: expected_args,
            }) if expected_interface == interface
                && expected_func == func
                && expected_args == args =>
            {
                None
            }
            Some(other) => Some(other.summary()),
        };
        if let Some(expected) = expected {
            self.diverge(expected, actual());
            return None;
        }

        self.position += 1;
        self.report.matched += 1;

        // The matching return is the very next event; a trace where it is not
        // is malformed rather than divergent.
        match self.events.get(self.position) {
            Some(Event::ImportReturn { outcome, .. }) => {
                let outcome = outcome.clone();
                self.position += 1;
                self.report.matched += 1;
                Some(outcome)
            }
            _ => {
                self.diverge(
                    format!("a recorded answer for {interface}#{func}"),
                    "the trace has none".to_string(),
                );
                None
            }
        }
    }

    /// The driver is about to make the export call recorded at `index`.
    ///
    /// Imports the guest makes from inside it are matched from the event after
    /// it, which is what the mock host's cursor is set to here.
    fn begin_export(&mut self, index: usize) {
        self.position = index + 1;
        self.report.matched += 1;
    }

    /// Judge what an export call produced against what the trace recorded for
    /// it. `expected` is `None` for a trace that has no return for the call,
    /// which is malformed rather than divergent but reported the same way.
    fn check_export_return(
        &mut self,
        func: &str,
        actual: &watoots::Result<Vec<String>>,
        expected: Option<&Outcome>,
    ) {
        match (actual, expected) {
            (Ok(values), Some(Outcome::Value(recorded))) => {
                let got = values.first();
                if got == recorded.as_ref() {
                    self.report.matched += 1;
                } else {
                    let or_none = |value: Option<&String>| {
                        value.map_or_else(|| "(no value)".to_string(), String::clone)
                    };
                    self.diverge(or_none(recorded.as_ref()), or_none(got));
                }
            }
            (Err(err), Some(Outcome::Error { status, .. })) => {
                if err.kind().name() == status {
                    self.report.matched += 1;
                } else {
                    self.diverge(status.clone(), err.kind().name().to_string());
                }
            }
            (Ok(values), Some(Outcome::Error { status, .. })) => {
                self.diverge(
                    format!("{func} to fail with {status}"),
                    format!("{func} returned {:?}", values.first()),
                );
            }
            (Err(err), Some(Outcome::Value(_))) => {
                self.diverge(
                    format!("{func} to return"),
                    format!("{func} failed: {}", err.message()),
                );
            }
            (_, None) => self.diverge(
                format!("a recorded return for {func}"),
                "the trace has none".to_string(),
            ),
        }
    }
}

/// Install the mock host: one function per import the component declares, each
/// answering from the trace instead of from the application.
///
/// Every declared import is served, not just the ones the recording exercised:
/// a plugin that declares an interface it did not call during this session
/// still has to link. Calling one the recording never saw is itself a
/// divergence. WASI imports are excluded; the host library serves those.
fn serve_imports(
    mut builder: watoots::HostBuilder,
    cursor: &Arc<Mutex<Cursor>>,
    declared: Vec<watoots::ImportedFunction>,
) -> watoots::HostBuilder {
    for imported in declared {
        if imported.interface.starts_with("wasi:") {
            continue;
        }
        let cursor = Arc::clone(cursor);
        let interface = imported.interface;
        let func = imported.func;
        let (interface_for_call, func_for_call) = (interface.clone(), func.clone());
        builder = builder.host_func(&interface, &func, move |call| {
            let args: Vec<String> = call
                .args()
                .iter()
                .map(|value| watoots::to_wave(value).unwrap_or_else(|_| UNREPRESENTABLE.into()))
                .collect();

            let mut cursor = cursor.lock().unwrap_or_else(|err| err.into_inner());
            let Some(outcome) = cursor.expect_import(&interface_for_call, &func_for_call, &args)
            else {
                // Diverged. Fail the guest's call rather than inventing an
                // answer: continuing would produce a cascade of consequences
                // that all trace back to this one point.
                return Err(HostError::internal(format!(
                    "replay diverged at {interface_for_call}#{func_for_call}"
                )));
            };

            match outcome {
                Outcome::Value(None) => Ok(Vec::new()),
                Outcome::Value(Some(text)) => {
                    let Some(ty) = call.result_types().first() else {
                        return Err(HostError::internal(format!(
                            "the trace records a value for {interface_for_call}#{func_for_call}, \
                             but the world declares no result"
                        )));
                    };
                    Ok(vec![watoots::from_wave(ty, &text)?])
                }
                Outcome::Error { status, message } => Err(HostError::new(
                    ErrorKind::from_name(&status).unwrap_or(ErrorKind::Internal),
                    message,
                )),
            }
        });
    }
    builder
}

/// Replay a trace against a component.
///
/// The application is not involved: imports are answered from the recording,
/// and exports are driven from it. Needs only the trace and the `.wasm` the
/// trace was recorded against — the manifest travels in the trace header.
pub fn replay(trace: &Trace, wasm: &[u8]) -> Result<ReplayReport> {
    if !trace.header.component_sha256.is_empty() {
        let actual = Trace::hash_component(wasm);
        if actual != trace.header.component_sha256 {
            return Err(Error::new(format!(
                "this trace was recorded against a different component\n  \
                 trace:     sha256:{}\n  component: sha256:{actual}",
                trace.header.component_sha256
            )));
        }
    }

    let cursor = Arc::new(Mutex::new(Cursor {
        events: trace.events.clone(),
        position: 0,
        report: ReplayReport::default(),
    }));

    let mut manifest = Manifest::parse(&trace.header.manifest_toml).map_err(Error::from)?;

    // Replay does not check signatures, and cannot: a trace carries the
    // manifest but not the signature, so a recording made under a
    // `[signature]` policy would be unreplayable — the one situation where a
    // bug report is worth the most is the one where it stopped working.
    //
    // Dropping it is safe in the way that matters and is worth being explicit
    // about. Every *permission* still applies, and the recorded limits and
    // determinism knobs still apply, which is why the manifest travels at all.
    // What is skipped is only "who published this", a question the person
    // replaying has already answered by choosing which bytes to hand the
    // replayer. See ADR-0014.
    manifest.signature.keys.clear();

    // The imports to serve come from the component, not the trace: see
    // `serve_imports`. Listing them needs an engine, so a bare host is built
    // for the question; the one that runs the replay is built afterwards, once
    // the mock functions are known.
    let probe = Host::builder().build().map_err(Error::from)?;
    let declared = probe.import_functions(wasm).map_err(Error::from)?;
    let builder = serve_imports(Host::builder().manifest(manifest), &cursor, declared);

    let host = builder.build().map_err(Error::from)?;
    let mut plugin = host
        .load_binary(&trace.header.plugin, wasm)
        .map_err(Error::from)?;

    // Drive the exports in recorded order.
    for (index, event) in trace.events.iter().enumerate() {
        let Event::ExportCall { func, args } = event else {
            continue;
        };

        cursor
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .begin_export(index);

        let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
        let actual = plugin.call_wave(func, &borrowed);

        // The recorded return for this call is the next return *of this
        // export*; everything between is the imports the guest made, which the
        // mock host has already consumed. Paired by name because that is what
        // the events carry; a trace with nested calls of one export would need
        // a structural pass, and WIT 0.2 has no reentrancy to produce one.
        let expected = trace.events[index + 1..]
            .iter()
            .find_map(|event| match event {
                Event::ExportReturn {
                    func: name,
                    outcome,
                } if name == func => Some(outcome),
                _ => None,
            });

        cursor
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .check_export_return(func, &actual, expected);
    }

    // The mock host functions hold their own clones of the cursor and outlive
    // this scope, so read the report out rather than trying to unwrap the Arc.
    let report = cursor
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .report
        .clone();
    Ok(report)
}
