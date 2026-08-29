//! Recording: a [`TraceHook`] that writes down every crossing.

use std::sync::Mutex;

use watoots::{Outcome as HostOutcome, TraceEvent, TraceHook, Val};

use crate::trace::{Event, Header, Outcome, Trace};
use crate::{Error, Result};

/// Records every host/plugin crossing into a [`Trace`].
///
/// Install it with `HostBuilder::trace_hook`. Recording costs one WAVE
/// rendering per value; nothing is written to disk until [`Recorder::finish`].
#[derive(Debug)]
pub struct Recorder {
    header: Header,
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    events: Vec<Event>,
    /// The first thing that made the recording untrustworthy, if any.
    ///
    /// A recorder cannot refuse mid-call — the guest is running and the host
    /// function has to answer — so a failure is remembered and reported by
    /// `finish`. Returning a partial trace as if it were complete is the one
    /// outcome that must not happen: a trace's whole value is that it is a
    /// faithful record.
    poisoned: Option<String>,
}

impl Recorder {
    /// Start recording against a component.
    #[must_use]
    pub fn new(header: Header) -> Self {
        Self {
            header,
            state: Mutex::new(State::default()),
        }
    }

    /// Take the recording.
    ///
    /// Fails if anything could not be represented, rather than handing back a
    /// trace with holes in it.
    pub fn finish(&self) -> Result<Trace> {
        let state = self.state.lock().unwrap_or_else(|err| err.into_inner());
        if let Some(reason) = &state.poisoned {
            return Err(Error::new(reason.clone()));
        }
        Ok(Trace {
            header: self.header.clone(),
            events: state.events.clone(),
        })
    }

    /// How many crossings have been recorded so far.
    #[must_use]
    pub fn len(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .events
            .len()
    }

    /// Whether nothing has been recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn push(&self, event: Event) {
        let mut state = self.state.lock().unwrap_or_else(|err| err.into_inner());
        state.events.push(event);
    }

    fn poison(&self, reason: String) {
        let mut state = self.state.lock().unwrap_or_else(|err| err.into_inner());
        if state.poisoned.is_none() {
            state.poisoned = Some(reason);
        }
    }

    fn encode(&self, values: &[Val]) -> Vec<String> {
        values
            .iter()
            .map(|value| match watoots::to_wave(value) {
                Ok(text) => text,
                Err(err) => {
                    // Resource handles are the expected case here: a handle is
                    // an index into a live table, not a value, so there is
                    // nothing truthful to write down. See ADR-0004.
                    self.poison(format!("this trace cannot be recorded: {}", err.message()));
                    "<unrepresentable>".to_string()
                }
            })
            .collect()
    }

    fn encode_outcome(&self, outcome: &HostOutcome<'_>) -> Outcome {
        match outcome {
            HostOutcome::Returned(values) => match values.first() {
                None => Outcome::Value(None),
                Some(value) => {
                    Outcome::Value(Some(self.encode(std::slice::from_ref(value)).remove(0)))
                }
            },
            HostOutcome::Failed(err) => Outcome::Error {
                status: err.kind().name().to_string(),
                message: err.message().to_string(),
            },
        }
    }
}

impl TraceHook for Recorder {
    fn on_event(&self, event: &TraceEvent<'_>) {
        let recorded = match event {
            TraceEvent::ExportCall { func, args, .. } => Event::ExportCall {
                func: (*func).to_string(),
                args: self.encode(args),
            },
            TraceEvent::ExportReturn { func, outcome, .. } => Event::ExportReturn {
                func: (*func).to_string(),
                outcome: self.encode_outcome(outcome),
            },
            TraceEvent::ImportCall {
                interface,
                func,
                args,
                ..
            } => Event::ImportCall {
                interface: (*interface).to_string(),
                func: (*func).to_string(),
                args: self.encode(args),
            },
            TraceEvent::ImportReturn {
                interface,
                func,
                outcome,
                ..
            } => Event::ImportReturn {
                interface: (*interface).to_string(),
                func: (*func).to_string(),
                outcome: self.encode_outcome(outcome),
            },
        };
        self.push(recorded);
    }
}
