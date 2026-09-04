//! Properties 2 and 3 of [ADR-0008]: a trace survives both encodings, and
//! recording a call sequence then replaying it reports faithful.
//!
//! Property 3 is the reason the ADR exists. A plugin host has no opinion about
//! what a plugin *should* return, so "wrong answer" is not a signal — but
//! "record it, replay it, and the two disagree" is, and it needs no knowledge
//! of the guest at all. It compares watoots against watoots.
//!
//! The component is WAT, so this suite needs no guest toolchain, like the rest
//! of the host's tests. See `crates/host/tests/logging.rs` for what the
//! index-based instance type is working around.
//!
//! [ADR-0008]: ../../../docs/adr/0008-fuzzing.md

use std::sync::{Arc, Mutex};

use proptest::prelude::*;
use watoots::fuzz::Generator;
use watoots::{Host, Manifest, TraceHook, Val};
use watoots_trace::{Event, Header, Outcome, Recorder, Trace, binary, replay, text};

/// Echoes a string, calls a host import, logs, and traps on demand.
///
/// Three exports on purpose. `run` takes a string and gives it straight back,
/// which puts a generated string through WAVE, through the trace's text
/// encoding, and back into a guest. `tally` returns whatever the host answered,
/// so a mock host's answer has to be parsed back into a value of the right
/// type. `boom` traps for odd arguments, so a recording can end in a failure
/// and a replay has to reproduce the failure rather than the value.
///
/// `run` also logs, for even `n` only. `wasi:logging` is served by the host
/// library rather than by replay's mock, so its crossings sit in the trace with
/// nothing consuming them and replay has to step over exactly those and no
/// others — the trickiest thing `replay.rs` does. Making it conditional means
/// the fuzzer varies where those crossings land in a sequence, which is the
/// part a fixed test cannot reach.
const ECHO: &str = r#"
(component
  (type (;0;)
    (instance
      (type (;0;) (func (param "text" string) (param "n" u32) (result u32)))
      (export (;0;) "note" (func (type 0)))
    )
  )
  (import "watoots:example/note@0.1.0" (instance $h (type 0)))
  (alias export $h "note" (func $note_fn))

  (type (;1;)
    (instance
      (type (;0;) (enum "trace" "debug" "info" "warn" "error" "critical"))
      (export (;1;) "level" (type (eq 0)))
      (type (;2;) (func (param "level" 1) (param "context" string) (param "message" string)))
      (export (;0;) "log" (func (type 2)))
    )
  )
  (import "wasi:logging/logging@0.1.0-draft" (instance $l (type 1)))
  (alias export $l "log" (func $log_fn))

  (core module $libc
    (memory (export "memory") 1)
    ;; A bump allocator that wraps rather than growing. One page is far more
    ;; than a bounded sequence of short strings needs, and wrapping keeps a long
    ;; campaign from ending in an out-of-bounds write instead of a finding.
    (global $next (mut i32) (i32.const 1024))
    (func (export "realloc") (param $old i32) (param $old_size i32) (param $align i32) (param $new_size i32) (result i32)
      (local $ret i32)
      (if (i32.gt_u (i32.add (global.get $next) (local.get $new_size)) (i32.const 60000))
        (then (global.set $next (i32.const 1024))))
      (local.set $ret (global.get $next))
      (global.set $next (i32.and (i32.add (i32.add (global.get $next) (local.get $new_size)) (i32.const 7)) (i32.const -8)))
      (local.get $ret))
  )
  (core instance $libc_i (instantiate $libc))
  (core func $note_lowered
    (canon lower (func $note_fn) (memory $libc_i "memory") (realloc (func $libc_i "realloc"))))
  (core func $log_lowered
    (canon lower (func $log_fn) (memory $libc_i "memory") (realloc (func $libc_i "realloc"))))

  (core module $m
    (import "h" "note" (func $note (param i32 i32 i32) (result i32)))
    (import "l" "log" (func $log (param i32 i32 i32 i32 i32)))
    (import "libc" "memory" (memory 1))
    (data (i32.const 8) "ctx")
    (data (i32.const 16) "tally")
    ;; A string result is two flat values, which is more than the canonical ABI
    ;; returns directly, so `run` writes {ptr, len} into a return area at 0 and
    ;; hands back its address.
    (func (export "run") (param $ptr i32) (param $len i32) (param $n i32) (result i32)
      (if (i32.eqz (i32.and (local.get $n) (i32.const 1)))
        (then (call $log (i32.const 3) (i32.const 8) (i32.const 3)
                         (local.get $ptr) (local.get $len))))
      (drop (call $note (local.get $ptr) (local.get $len) (local.get $n)))
      (i32.store (i32.const 0) (local.get $ptr))
      (i32.store (i32.const 4) (local.get $len))
      (i32.const 0))
    (func (export "tally") (param $n i32) (result i32)
      (call $note (i32.const 16) (i32.const 5) (local.get $n)))
    (func (export "boom") (param $n i32) (result i32)
      (if (i32.and (local.get $n) (i32.const 1)) (then unreachable))
      (local.get $n))
  )
  (core instance $note_i (export "note" (func $note_lowered)))
  (core instance $log_i (export "log" (func $log_lowered)))
  (core instance $i (instantiate $m
    (with "h" (instance $note_i))
    (with "l" (instance $log_i))
    (with "libc" (instance $libc_i))))

  (func $run (param "text" string) (param "n" u32) (result string)
    (canon lift (core func $i "run") (memory $libc_i "memory") (realloc (func $libc_i "realloc"))))
  (func $tally (param "n" u32) (result u32) (canon lift (core func $i "tally")))
  (func $boom (param "n" u32) (result u32) (canon lift (core func $i "boom")))
  (export "run" (func $run))
  (export "tally" (func $tally))
  (export "boom" (func $boom))
)
"#;

const NOTE: &str = "watoots:example/note@0.1.0";

/// The policy the echo component runs under, which has to grant logging or the
/// component does not load at all. It travels in the trace header, and replay
/// rebuilds its host from it.
const POLICY: &str = "[permissions]\nlogging = \"trace\"\n\n[limits]\nfuel = 50_000_000\n";

/// The case budget, overridable for a longer local run.
///
/// Property 3 costs an engine, three component compiles and an instantiation
/// per case, so its default is small deliberately: `cargo test --workspace` is
/// something a contributor runs without thinking, and a real campaign is
/// `watoots fuzz`.
fn config(default_cases: u32) -> ProptestConfig {
    let cases = std::env::var("WATOOTS_PROPTEST_CASES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default_cases);
    ProptestConfig {
        cases,
        ..ProptestConfig::default()
    }
}

// ---------------------------------------------------------------------------
// Property 3: record, replay, and the two must agree.
// ---------------------------------------------------------------------------

/// One recorded session against the echo component.
///
/// Returns the trace and what happened, so the caller can assert on both. The
/// sequence stops at the first failing call: a trapped component instance
/// cannot be re-entered, so anything after a trap would be recording the
/// engine's refusal rather than the plugin.
fn record_a_session(bytes: Vec<u8>, calls: usize) -> Trace {
    let wasm = ECHO.as_bytes();

    let recorder = Arc::new(Recorder::new(Header {
        component_sha256: Trace::hash_component(wasm),
        plugin: "echo".to_string(),
        manifest_toml: POLICY.to_string(),
    }));

    // One generator drives both the arguments and the host's answers, so a
    // shrunk entropy buffer shrinks the whole session rather than half of it.
    let generator = Arc::new(Mutex::new(Generator::from_bytes(bytes).max_elements(6)));
    let answers = Arc::clone(&generator);

    let host = Host::builder()
        .manifest(Manifest::parse(POLICY).unwrap())
        .host_func(NOTE, "note", move |call| {
            let mut generator = answers.lock().unwrap_or_else(|err| err.into_inner());
            generator.values(call.result_types())
        })
        .trace_hook(Arc::clone(&recorder) as Arc<dyn TraceHook>)
        .build()
        .unwrap();

    let mut plugin = host.load_binary("echo", wasm).unwrap();

    for _ in 0..calls {
        let export = {
            let mut generator = generator.lock().unwrap_or_else(|err| err.into_inner());
            ["run", "tally", "boom"][pick(&mut generator, 3)]
        };
        let params = plugin.export_params(export).unwrap();
        let args: Vec<Val> = {
            let mut generator = generator.lock().unwrap_or_else(|err| err.into_inner());
            generator.values(&params).unwrap()
        };
        if plugin.call(export, &args).is_err() {
            // A trapped instance refuses re-entry, so the session is over.
            break;
        }
    }

    recorder
        .finish()
        .expect("nothing in this world is a resource")
}

/// Draw a small number without exposing the generator's internals: generating a
/// value of a type whose only inhabitant is a number is the public way in.
fn pick(generator: &mut Generator, bound: usize) -> usize {
    let Ok(Val::U8(byte)) = generator.value(&watoots::Type::U8) else {
        return 0;
    };
    usize::from(byte) % bound
}

proptest! {
    #![proptest_config(config(32))]

    /// Property 3, and the point of the exercise: recording a call sequence and
    /// replaying it reports faithful.
    ///
    /// Nothing here knows what the plugin ought to do. It compares watoots
    /// against watoots, so the same oracle works on a plugin nobody has seen.
    #[test]
    fn a_recorded_session_replays_faithfully(
        bytes in proptest::collection::vec(any::<u8>(), 0..512),
        calls in 1usize..5,
    ) {
        let trace = record_a_session(bytes, calls);
        prop_assert!(!trace.events.is_empty(), "the session recorded nothing");

        let report = replay(&trace, ECHO.as_bytes())
            .map_err(|err| TestCaseError::fail(err.message().to_string()))?;

        prop_assert!(
            report.is_faithful(),
            "{}\n--- the trace ---\n{}",
            report.describe(),
            text::to_text(&trace)
        );
    }

    /// Property 2 against traces that are real rather than constructed: the
    /// same session, taken out through text and through the binary framing,
    /// still replays.
    ///
    /// This is what a bug report is — a file somebody attached — so the
    /// encodings have to be lossless for the *whole* trace, header included.
    #[test]
    fn a_recorded_session_survives_both_encodings(
        bytes in proptest::collection::vec(any::<u8>(), 0..512),
        calls in 1usize..5,
    ) {
        let trace = record_a_session(bytes, calls);

        let via_text = text::from_text(&text::to_text(&trace))
            .map_err(|err| TestCaseError::fail(err.message().to_string()))?;
        prop_assert_eq!(&via_text, &trace, "the text encoding lost something");

        let via_binary = binary::from_bytes(&binary::to_bytes(&trace))
            .map_err(|err| TestCaseError::fail(err.message().to_string()))?;
        prop_assert_eq!(&via_binary, &trace, "the binary encoding lost something");

        // ADR-0008 words it as text → binary → text, which is what
        // `watoots trace fmt` actually does to a file.
        let round_tripped = binary::from_bytes(&binary::to_bytes(&via_text))
            .map_err(|err| TestCaseError::fail(err.message().to_string()))?;
        prop_assert_eq!(text::to_text(&round_tripped), text::to_text(&trace));

        let report = replay(&via_text, ECHO.as_bytes())
            .map_err(|err| TestCaseError::fail(err.message().to_string()))?;
        prop_assert!(report.is_faithful(), "{}", report.describe());
    }
}

// ---------------------------------------------------------------------------
// Property 2: the encodings, against traces built rather than recorded.
// ---------------------------------------------------------------------------

/// Strings that have broken a line-oriented format before, or look like they
/// might. The trace text encoding parses a line by its first word and holds one
/// value per line, so keyword-shaped text is where it is most likely to come
/// apart.
///
/// Empty and whitespace-padded values are excluded, and that exclusion is a
/// *finding* rather than a convenience: see
/// `the_text_encoding_cannot_carry_an_empty_or_space_padded_value`. Nothing
/// `to_wave` produces is empty or padded — the smallest renderings are `""`,
/// `[]` and `{}` — so a recorded trace never contains one, and this strategy
/// generates the values a recording can actually hold.
fn awkward() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("arg \"x\"".to_string()),
        Just("value 1".to_string()),
        Just("unit".to_string()),
        Just("error WT_ERR_TRAP \"boom\"".to_string()),
        Just("watoots-trace 1".to_string()),
        Just("\"a\\nb\"".to_string()),
        Just("{write, exec}".to_string()),
        Just("[{line: 1, message: \"unresolved TODO\"}]".to_string()),
        ".{0,24}",
    ]
    .prop_filter(
        "the text encoding cannot carry an empty or whitespace-padded value",
        |value| {
            !value.is_empty()
                && !value.starts_with(char::is_whitespace)
                && !value.ends_with(char::is_whitespace)
        },
    )
}

/// An error message. Unrestricted on purpose: messages are quoted and escaped
/// rather than written raw, so this is where newlines, quotes and backslashes
/// *are* meant to survive — and `Plugin::classify_call_error` puts a whole
/// guest backtrace in one, so multi-line messages are the normal case.
fn message() -> impl Strategy<Value = String> {
    prop_oneof![
        Just(String::new()),
        Just("trap:\n  \"unreachable\"".to_string()),
        Just("a\\b".to_string()),
        Just("ends in a space ".to_string()),
        "(?s).{0,48}",
    ]
}

/// A function or interface name: no whitespace, because the text encoding
/// splits an event's line on it. A WIT identifier never contains any.
fn wit_name() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("run".to_string()),
        Just("lint".to_string()),
        Just("watoots:example/log@0.1.0".to_string()),
        Just("wasi:logging/logging@0.1.0-draft".to_string()),
        "[a-z][a-z0-9-]{0,12}",
    ]
}

fn outcome() -> impl Strategy<Value = Outcome> {
    prop_oneof![
        Just(Outcome::Value(None)),
        awkward().prop_map(|value| Outcome::Value(Some(value))),
        (wit_name(), message()).prop_map(|(status, message)| Outcome::Error {
            status: format!("WT_ERR_{}", status.to_uppercase().replace('-', "_")),
            message,
        }),
    ]
}

fn event() -> impl Strategy<Value = Event> {
    prop_oneof![
        (wit_name(), proptest::collection::vec(awkward(), 0..4))
            .prop_map(|(func, args)| Event::ExportCall { func, args }),
        (wit_name(), outcome()).prop_map(|(func, outcome)| Event::ExportReturn { func, outcome }),
        (
            wit_name(),
            wit_name(),
            proptest::collection::vec(awkward(), 0..4)
        )
            .prop_map(|(interface, func, args)| Event::ImportCall {
                interface,
                func,
                args
            }),
        (wit_name(), wit_name(), outcome()).prop_map(|(interface, func, outcome)| {
            Event::ImportReturn {
                interface,
                func,
                outcome,
            }
        }),
    ]
}

/// A manifest as it comes off disk.
///
/// Every case here either is empty or ends in a newline, and that too is a
/// finding rather than a convenience:
/// `the_text_encoding_normalises_a_manifest_it_re_indents` records what happens
/// to one that does not.
fn manifest_toml() -> impl Strategy<Value = String> {
    prop_oneof![
        Just(String::new()),
        Just("[permissions]\n".to_string()),
        Just("[permissions]\nlogging = \"trace\"\n".to_string()),
        Just("[permissions]\nfs = { read = [\"/tmp\"] }\n\n[limits]\nfuel = 1\n".to_string()),
        "([a-z]+ = [0-9]+\n){0,4}",
    ]
}

fn trace() -> impl Strategy<Value = Trace> {
    (
        "[0-9a-f]{0,64}",
        "[a-z_][a-z0-9_.-]{0,16}",
        manifest_toml(),
        proptest::collection::vec(event(), 0..12),
    )
        .prop_map(|(component_sha256, plugin, manifest_toml, events)| Trace {
            header: Header {
                component_sha256,
                plugin,
                manifest_toml,
            },
            events,
        })
}

proptest! {
    #![proptest_config(config(512))]

    /// Property 2: a trace survives text → binary → text unchanged, and, more
    /// strongly, survives each encoding on its own.
    #[test]
    fn a_trace_survives_both_encodings(trace in trace()) {
        let via_text = text::from_text(&text::to_text(&trace))
            .map_err(|err| TestCaseError::fail(err.message().to_string()))?;
        prop_assert_eq!(&via_text, &trace);

        let via_binary = binary::from_bytes(&binary::to_bytes(&trace))
            .map_err(|err| TestCaseError::fail(err.message().to_string()))?;
        prop_assert_eq!(&via_binary, &trace);

        let there_and_back = binary::from_bytes(&binary::to_bytes(&via_text))
            .map_err(|err| TestCaseError::fail(err.message().to_string()))?;
        prop_assert_eq!(text::to_text(&there_and_back), text::to_text(&trace));
    }

    /// Property 4, on the two trace parsers. Both read files, which may be
    /// truncated or hostile; `Err` is a fine answer and a panic is not.
    #[test]
    fn reading_arbitrary_input_as_a_trace_never_panics(
        text_input in ".{0,256}",
        bytes in proptest::collection::vec(any::<u8>(), 0..256),
    ) {
        let _ = text::from_text(&text_input);
        let _ = binary::from_bytes(&bytes);
        // A real header in front of nonsense is the more interesting shape:
        // it gets past the magic and into the event loop.
        let mut framed = b"watoots-trace 1\n".to_vec();
        framed.extend_from_slice(text_input.as_bytes());
        let _ = text::from_text(&String::from_utf8_lossy(&framed));

        let mut binary_framed = b"WTTR\x01\x00\x00\x00".to_vec();
        binary_framed.extend_from_slice(&bytes);
        let _ = binary::from_bytes(&binary_framed);
    }

    /// Property 4, on replay. A hand-edited trace is a supported thing to have
    /// — the text encoding exists so a reviewer can construct a case that never
    /// happened — so replay has to answer, not crash.
    #[test]
    fn replaying_an_edited_trace_never_panics(mut trace in trace()) {
        trace.header.component_sha256 = Trace::hash_component(ECHO.as_bytes());
        trace.header.manifest_toml = POLICY.to_string();
        let _ = replay(&trace, ECHO.as_bytes());
    }
}

// ---------------------------------------------------------------------------
// What the properties found in the text encoding.
//
// Both of these are round-trip losses in `text.rs`, found by
// `a_trace_survives_both_encodings` and left in place rather than corrected:
// changing either one changes the trace format, and neither can arise from a
// recording. They are written down here so that the narrowing in `awkward()`
// and `manifest_toml()` reads as a known limit rather than as a generator that
// happens to avoid the bug.
// ---------------------------------------------------------------------------

#[test]
fn the_text_encoding_cannot_carry_an_empty_or_space_padded_value() {
    // `to_text` writes `  arg {value}`; `from_text` reads the line back with
    // `trim().strip_prefix("arg ")`. For an empty value that leaves the bare
    // word `arg`, which the outer loop then rejects as an unknown keyword — so
    // the encoder produces a file its own parser refuses. A value padded with
    // whitespace loses the padding to the same `trim`, and to the `trim_start`
    // that `split_once` does on the rest of a `value` line.
    //
    // Unreachable from a recording: `to_wave` has no empty rendering (the
    // shortest are `""`, `[]` and `{}`) and never emits trailing whitespace, so
    // every argument in a real trace is safe. It is reachable by hand-editing a
    // trace, which the text encoding exists to invite — and there it fails
    // loudly with a line number rather than silently, which is the right
    // failure to have if you have to have one.
    let empty = Trace {
        header: Header::default(),
        events: vec![Event::ExportCall {
            func: "run".to_string(),
            args: vec![String::new()],
        }],
    };
    let err = text::from_text(&text::to_text(&empty)).unwrap_err();
    assert!(
        err.message().contains("unknown keyword"),
        "{}",
        err.message()
    );

    let padded = Trace {
        header: Header::default(),
        events: vec![Event::ExportReturn {
            func: "run".to_string(),
            outcome: Outcome::Value(Some("  1 ".to_string())),
        }],
    };
    let back = text::from_text(&text::to_text(&padded)).unwrap();
    assert_eq!(
        back.events[0],
        Event::ExportReturn {
            func: "run".to_string(),
            outcome: Outcome::Value(Some("1".to_string())),
        }
    );

    // The binary framing has neither problem: it is length-prefixed.
    assert_eq!(
        binary::from_bytes(&binary::to_bytes(&empty)).unwrap(),
        empty
    );
    assert_eq!(
        binary::from_bytes(&binary::to_bytes(&padded)).unwrap(),
        padded
    );
}

#[test]
fn the_text_encoding_normalises_a_manifest_it_re_indents() {
    // The manifest is written out two-space indented and read back a line at a
    // time with a `\n` appended to each, so a manifest that did not end in a
    // newline gains one, and a manifest that is nothing but whitespace
    // disappears (`to_text` skips a blank block). This one *is* reachable:
    // `watoots record -m policy.toml` puts the file's bytes in the header
    // verbatim, and a TOML file with no final newline is ordinary.
    //
    // Left alone because it is lossless where it counts. The header exists so
    // replay can rebuild the host, and it does that through `Manifest::parse`,
    // which cannot tell the two spellings apart.
    let unterminated = Trace {
        header: Header {
            manifest_toml: "[limits]\nfuel = 1".to_string(),
            ..Header::default()
        },
        events: Vec::new(),
    };
    let back = text::from_text(&text::to_text(&unterminated)).unwrap();
    assert_eq!(back.header.manifest_toml, "[limits]\nfuel = 1\n");
    assert_eq!(
        Manifest::parse(&back.header.manifest_toml)
            .unwrap()
            .limits
            .fuel,
        Manifest::parse(&unterminated.header.manifest_toml)
            .unwrap()
            .limits
            .fuel,
        "the two spellings have to mean the same policy, or replay is not \
         reproducing the run it claims to"
    );

    let blank = Trace {
        header: Header {
            manifest_toml: "   ".to_string(),
            ..Header::default()
        },
        events: Vec::new(),
    };
    assert_eq!(
        text::from_text(&text::to_text(&blank))
            .unwrap()
            .header
            .manifest_toml,
        ""
    );

    // Again, the binary framing keeps the bytes it was given.
    assert_eq!(
        binary::from_bytes(&binary::to_bytes(&unterminated)).unwrap(),
        unterminated
    );
}

#[test]
fn a_trapping_call_replays_as_a_failure() {
    // The oracle has to be able to say "it failed the same way", not only "it
    // returned the same value", or half of what a bug report contains would go
    // unchecked.
    let mut generator = Generator::from_seed(1);
    let _ = &mut generator;

    let wasm = ECHO.as_bytes();
    let recorder = Arc::new(Recorder::new(Header {
        component_sha256: Trace::hash_component(wasm),
        plugin: "echo".to_string(),
        manifest_toml: POLICY.to_string(),
    }));
    let host = Host::builder()
        .manifest(Manifest::parse(POLICY).unwrap())
        .host_func(NOTE, "note", |_| Ok(vec![Val::U32(0)]))
        .trace_hook(Arc::clone(&recorder) as Arc<dyn TraceHook>)
        .build()
        .unwrap();

    let mut plugin = host.load_binary("echo", wasm).unwrap();
    assert!(plugin.call("boom", &[Val::U32(3)]).is_err());

    let trace = recorder.finish().unwrap();
    assert!(
        matches!(
            trace.events.last(),
            Some(Event::ExportReturn {
                outcome: Outcome::Error { status, .. },
                ..
            }) if status == "WT_ERR_TRAP"
        ),
        "{:?}",
        trace.events.last()
    );

    let report = replay(&trace, wasm).unwrap();
    assert!(report.is_faithful(), "{}", report.describe());
}
