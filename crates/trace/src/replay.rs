//! Replay: run the component again with the application replaced by the trace.

use std::sync::{Arc, Mutex};

use watoots::{Error as HostError, ErrorKind, Host, Manifest};

use crate::trace::{Event, Outcome, Trace};
use crate::{Error, Result};

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

    /// The next import the trace expects, and the answer it recorded.
    fn expect_import(&mut self, interface: &str, func: &str, args: &[String]) -> Option<Outcome> {
        let actual = format!("{interface}#{func}({})", args.join(", "));

        let Some(event) = self.events.get(self.position).cloned() else {
            self.diverge("the trace to be over".to_string(), actual);
            return None;
        };

        let Event::ImportCall {
            interface: expected_interface,
            func: expected_func,
            args: expected_args,
        } = event
        else {
            self.diverge(self.events[self.position].summary(), actual);
            return None;
        };

        if expected_interface != interface || expected_func != func || expected_args != args {
            self.diverge(
                format!(
                    "{expected_interface}#{expected_func}({})",
                    expected_args.join(", ")
                ),
                actual,
            );
            return None;
        }

        self.position += 1;
        self.report.matched += 1;

        // The matching return is the very next event; a trace where it is not
        // is malformed rather than divergent.
        match self.events.get(self.position).cloned() {
            Some(Event::ImportReturn { outcome, .. }) => {
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

    let manifest = Manifest::parse(&trace.header.manifest_toml).map_err(Error::from)?;

    // Serve every host import the component *declares*, not just the ones the
    // recording happened to exercise. A plugin that declares an interface it
    // did not call during this session still has to link, or replaying a
    // recording of one export would fail because a different one was never
    // reached. WASI imports are excluded: the host library serves those.
    let probe = Host::builder().build().map_err(Error::from)?;
    let declared = probe.import_functions(wasm).map_err(Error::from)?;
    let to_serve: Vec<(String, String)> = declared
        .into_iter()
        .filter(|imported| !imported.interface.starts_with("wasi:"))
        .map(|imported| (imported.interface, imported.func))
        .collect();

    let mut builder = Host::builder().manifest(manifest);

    // The mock host: each function answers from the trace instead of from the
    // application. One that the recording never saw called is still served, and
    // calling it now is itself a divergence.
    for (interface, func) in to_serve {
        let cursor = Arc::clone(&cursor);
        let interface_for_call = interface.clone();
        let func_for_call = func.clone();
        builder = builder.host_func(&interface, &func, move |call| {
            let args: Vec<String> = call
                .args()
                .iter()
                .map(|value| watoots::to_wave(value).unwrap_or_else(|_| "<unrepresentable>".into()))
                .collect();

            let mut cursor = cursor.lock().unwrap_or_else(|err| err.into_inner());
            let Some(outcome) = cursor.expect_import(&interface_for_call, &func_for_call, &args)
            else {
                // Diverged. Fail the guest's call rather than inventing an
                // answer: continuing would produce a cascade of consequences
                // that all trace back to this one point.
                return Err(HostError::new(
                    ErrorKind::Internal,
                    format!("replay diverged at {interface_for_call}#{func_for_call}"),
                ));
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

    let host = builder.build().map_err(Error::from)?;
    let mut plugin = host
        .load_binary(&trace.header.plugin, wasm)
        .map_err(Error::from)?;

    // Drive the exports in recorded order.
    let mut index = 0;
    while index < trace.events.len() {
        let Event::ExportCall { func, args } = &trace.events[index] else {
            index += 1;
            continue;
        };

        {
            let mut guard = cursor.lock().unwrap_or_else(|err| err.into_inner());
            guard.position = index + 1;
            guard.report.matched += 1;
        }

        let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
        let actual = plugin.call_wave(func, &borrowed);

        // Find the recorded return for this call: everything between is the
        // imports the guest made, which the mock host has already consumed.
        let expected = trace.events[index + 1..]
            .iter()
            .find_map(|event| match event {
                Event::ExportReturn {
                    func: name,
                    outcome,
                } if name == func => Some(outcome.clone()),
                _ => None,
            });

        let mut guard = cursor.lock().unwrap_or_else(|err| err.into_inner());
        match (&actual, &expected) {
            (Ok(values), Some(Outcome::Value(recorded))) => {
                let got = values.first().cloned();
                if got == *recorded {
                    guard.report.matched += 1;
                } else {
                    guard.diverge(
                        recorded.clone().unwrap_or_else(|| "(no value)".into()),
                        got.unwrap_or_else(|| "(no value)".into()),
                    );
                }
            }
            (Err(err), Some(Outcome::Error { status, .. })) => {
                if err.kind().name() == status {
                    guard.report.matched += 1;
                } else {
                    guard.diverge(status.clone(), err.kind().name().to_string());
                }
            }
            (Ok(values), Some(Outcome::Error { status, .. })) => {
                guard.diverge(
                    format!("{func} to fail with {status}"),
                    format!("{func} returned {:?}", values.first()),
                );
            }
            (Err(err), Some(Outcome::Value(_))) => {
                guard.diverge(
                    format!("{func} to return"),
                    format!("{func} failed: {}", err.message()),
                );
            }
            (_, None) => guard.diverge(
                format!("a recorded return for {func}"),
                "the trace has none".to_string(),
            ),
        }
        drop(guard);

        index += 1;
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
