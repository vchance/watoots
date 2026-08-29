//! Record a real session, then reproduce it with the application gone.

use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};

use watoots::{Host, Manifest, TraceHook, Val};
use watoots_trace::{Event, Header, Outcome, Recorder, Trace, binary, replay, text};

/// The policy a Rust guest needs: std imports the clock and the environment.
const POLICY: &str = r#"
[permissions]
clocks = "monotonic"
env    = {}

[limits]
fuel = 200_000_000
"#;

fn sample_plugin() -> &'static PathBuf {
    static ARTIFACT: OnceLock<PathBuf> = OnceLock::new();
    ARTIFACT.get_or_init(|| {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let status = Command::new(env!("CARGO"))
            .args(["build", "--manifest-path"])
            .arg(root.join("examples/plugins/rust-lint/Cargo.toml"))
            .args(["--target", "wasm32-wasip2", "--release"])
            .status()
            .expect("running cargo to build the sample plugin");
        assert!(status.success(), "failed to build the sample plugin");
        root.join("examples/plugins/rust-lint/target/wasm32-wasip2/release/rust_lint.wasm")
    })
}

/// Record one `lint` call against the real plugin.
fn record_a_session() -> (Trace, Vec<u8>) {
    let wasm = std::fs::read(sample_plugin()).unwrap();

    let recorder = Arc::new(Recorder::new(Header {
        component_sha256: Trace::hash_component(&wasm),
        plugin: "rust_lint".to_string(),
        manifest_toml: POLICY.to_string(),
    }));

    let host = Host::builder()
        .manifest(Manifest::parse(POLICY).unwrap())
        .host_func("watoots:example/log@0.1.0", "emit", |_call| Ok(Vec::new()))
        .trace_hook(Arc::clone(&recorder) as Arc<dyn TraceHook>)
        .build()
        .unwrap();

    let mut plugin = host.load_binary("rust_lint", &wasm).unwrap();
    plugin
        .call(
            "lint",
            &[
                Val::String("notes.md".to_string()),
                Val::String("TODO: fix\n".to_string()),
            ],
        )
        .unwrap();

    (recorder.finish().unwrap(), wasm)
}

#[test]
fn recording_captures_both_directions_in_order() {
    let (trace, _) = record_a_session();

    let shape: Vec<&str> = trace
        .events
        .iter()
        .map(|event| match event {
            Event::ExportCall { .. } => "export-call",
            Event::ImportCall { .. } => "import-call",
            Event::ImportReturn { .. } => "import-return",
            Event::ExportReturn { .. } => "export-return",
        })
        .collect();

    assert_eq!(
        shape,
        [
            "export-call",
            "import-call",
            "import-return",
            "import-call",
            "import-return",
            "export-return",
        ]
    );
}

#[test]
fn a_recorded_trace_is_readable() {
    let (trace, _) = record_a_session();
    let rendered = text::to_text(&trace);

    // The whole reason for a WIT-level format: this is legible, and a reviewer
    // can see what crossed without a decoder.
    assert!(rendered.contains("export-call lint"), "{rendered}");
    assert!(rendered.contains(r#"arg "notes.md""#), "{rendered}");
    assert!(
        rendered.contains("import-call watoots:example/log@0.1.0 emit"),
        "{rendered}"
    );
    assert!(rendered.contains("unresolved TODO"), "{rendered}");
}

#[test]
fn a_recorded_trace_survives_both_encodings() {
    let (trace, _) = record_a_session();
    assert_eq!(text::from_text(&text::to_text(&trace)).unwrap(), trace);
    assert_eq!(
        binary::from_bytes(&binary::to_bytes(&trace)).unwrap(),
        trace
    );
}

#[test]
fn the_manifest_travels_with_the_trace() {
    // Replay needs the policy that was in force, and demanding the application
    // supply it again would defeat the point of a self-contained fixture.
    let (trace, _) = record_a_session();
    assert!(trace.header.manifest_toml.contains("clocks"));
    Manifest::parse(&trace.header.manifest_toml).unwrap();
}

#[test]
fn replay_reproduces_the_session_without_the_application() {
    let (trace, wasm) = record_a_session();

    // Note what is *not* here: no host functions, no manifest, no application.
    // Only the trace and the component.
    let report = replay(&trace, &wasm).unwrap();

    assert!(report.is_faithful(), "{}", report.describe());
    assert_eq!(report.matched, trace.events.len());
}

#[test]
fn replay_survives_a_round_trip_through_text() {
    // What a bug report actually looks like: a file someone attached.
    let (trace, wasm) = record_a_session();
    let from_file = text::from_text(&text::to_text(&trace)).unwrap();

    let report = replay(&from_file, &wasm).unwrap();
    assert!(report.is_faithful(), "{}", report.describe());
}

#[test]
fn a_changed_argument_is_caught_as_a_divergence() {
    let (mut trace, wasm) = record_a_session();

    // Edit the trace so it claims the plugin logged something it did not. This
    // is also how you would hand-write a case that never happened.
    for event in &mut trace.events {
        if let Event::ImportCall { args, .. } = event {
            args[1] = "\"something else entirely\"".to_string();
            break;
        }
    }

    let report = replay(&trace, &wasm).unwrap();
    assert!(!report.is_faithful(), "the edit should have been caught");

    let first = &report.divergences[0];
    assert!(
        first.expected.contains("something else entirely"),
        "{first}"
    );
    assert!(first.actual.contains("linting notes.md"), "{first}");
    assert!(
        report.describe().contains("diverged"),
        "{}",
        report.describe()
    );
}

#[test]
fn a_changed_return_value_is_caught_as_a_divergence() {
    let (mut trace, wasm) = record_a_session();

    for event in &mut trace.events {
        if let Event::ExportReturn { outcome, .. } = event {
            *outcome = Outcome::Value(Some("[]".to_string()));
        }
    }

    let report = replay(&trace, &wasm).unwrap();
    assert!(!report.is_faithful(), "{}", report.describe());
    assert_eq!(report.divergences[0].expected, "[]");
    assert!(
        report.divergences[0].actual.contains("unresolved TODO"),
        "{}",
        report.divergences[0]
    );
}

#[test]
fn replaying_against_the_wrong_component_is_refused() {
    let (trace, _) = record_a_session();

    let other = br#"(component)"#;
    let err = replay(&trace, other).unwrap_err();
    assert!(
        err.message().contains("different component"),
        "{}",
        err.message()
    );
}

#[test]
fn a_recorder_can_be_shared_across_plugins() {
    // One recorder, two plugins: the events carry the plugin they belong to at
    // the host level, and the trace header names the one being recorded.
    let (trace, _) = record_a_session();
    assert_eq!(trace.header.plugin, "rust_lint");
    assert!(!trace.events.is_empty());
}

#[test]
fn an_empty_recording_is_still_a_valid_trace() {
    let recorder = Recorder::new(Header::default());
    assert!(recorder.is_empty());
    let trace = recorder.finish().unwrap();
    assert_eq!(text::from_text(&text::to_text(&trace)).unwrap(), trace);
}

/// A collector used to prove the recorder does not swallow a failing call.
#[derive(Default)]
struct Counter(Mutex<usize>);

impl TraceHook for Counter {
    fn on_event(&self, _event: &watoots::TraceEvent<'_>) {
        *self.0.lock().unwrap() += 1;
    }
}

#[test]
fn a_failing_export_is_recorded_as_an_error() {
    let wasm = std::fs::read(sample_plugin()).unwrap();
    let recorder = Arc::new(Recorder::new(Header {
        component_sha256: Trace::hash_component(&wasm),
        plugin: "rust_lint".to_string(),
        manifest_toml: POLICY.to_string(),
    }));
    let counter = Arc::new(Counter::default());
    let _ = &counter;

    // A host function that refuses: the guest's call fails, and the trace has
    // to say so rather than silently ending.
    let host = Host::builder()
        .manifest(Manifest::parse(POLICY).unwrap())
        .host_func("watoots:example/log@0.1.0", "emit", |_call| {
            Err(watoots::Error::internal("the host said no"))
        })
        .trace_hook(Arc::clone(&recorder) as Arc<dyn TraceHook>)
        .build()
        .unwrap();

    let mut plugin = host.load_binary("rust_lint", &wasm).unwrap();
    let outcome = plugin.call(
        "lint",
        &[
            Val::String("a.md".to_string()),
            Val::String("TODO\n".to_string()),
        ],
    );
    assert!(outcome.is_err());

    let trace = recorder.finish().unwrap();
    let last = trace.events.last().unwrap();
    assert!(
        matches!(
            last,
            Event::ExportReturn {
                outcome: Outcome::Error { .. },
                ..
            }
        ),
        "{last:?}"
    );
}

#[test]
fn replay_serves_imports_the_recording_never_exercised() {
    // Recording `name` never calls `emit`, so the trace has no import events
    // at all. Replay still has to serve the interface the component declares,
    // or a plugin would only be replayable through whichever export happened to
    // touch every one of its imports.
    let wasm = std::fs::read(sample_plugin()).unwrap();
    let recorder = Arc::new(Recorder::new(Header {
        component_sha256: Trace::hash_component(&wasm),
        plugin: "rust_lint".to_string(),
        manifest_toml: POLICY.to_string(),
    }));

    let host = Host::builder()
        .manifest(Manifest::parse(POLICY).unwrap())
        .host_func("watoots:example/log@0.1.0", "emit", |_call| Ok(Vec::new()))
        .trace_hook(Arc::clone(&recorder) as Arc<dyn TraceHook>)
        .build()
        .unwrap();

    let mut plugin = host.load_binary("rust_lint", &wasm).unwrap();
    plugin.call("name", &[]).unwrap();

    let trace = recorder.finish().unwrap();
    assert!(
        trace.imported_functions().is_empty(),
        "this recording should contain no import crossings"
    );

    let report = replay(&trace, &wasm).unwrap();
    assert!(report.is_faithful(), "{}", report.describe());
}

#[test]
fn the_determinism_settings_travel_with_the_trace() {
    // Replay rebuilds its host from the trace header. If the settings did not
    // travel, a divergence report could be a statement about the engine rather
    // than about the plugin.
    let (trace, wasm) = record_a_session();
    let manifest = Manifest::parse(&trace.header.manifest_toml).unwrap();
    assert!(manifest.determinism.enabled);

    let report = replay(&trace, &wasm).unwrap();
    assert!(report.is_faithful(), "{}", report.describe());
}
