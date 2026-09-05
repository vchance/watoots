//! The Rust sample plugin, end to end.
//!
//! Everything else in the suite uses hand-written WAT. This one drives the real
//! artifact from `examples/plugins/rust-lint`, compiled by wit-bindgen against
//! the WIT world in `examples/wit/lint` — so it is the test that says the typed
//! boundary actually works in both directions: the host serves an import, the
//! plugin exports records and lists, and neither side agreed on a wire format.

use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};

use watoots::{Host, Manifest, Outcome, Registry, TraceEvent, TraceHook, Val};

/// Build the sample plugin once per test binary and return the artifact path.
///
/// It has its own workspace and target directory, so this does not contend with
/// the outer build's lock.
fn sample_plugin() -> &'static PathBuf {
    static ARTIFACT: OnceLock<PathBuf> = OnceLock::new();
    ARTIFACT.get_or_init(|| {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let manifest = root.join("examples/plugins/rust-lint/Cargo.toml");

        let status = Command::new(env!("CARGO"))
            .args(["build", "--manifest-path"])
            .arg(&manifest)
            .args(["--target", "wasm32-wasip2", "--release"])
            .status()
            .expect("running cargo to build the sample plugin");
        assert!(
            status.success(),
            "failed to build the sample plugin; is the wasm32-wasip2 target installed?"
        );

        let artifact =
            root.join("examples/plugins/rust-lint/target/wasm32-wasip2/release/rust_lint.wasm");
        assert!(artifact.is_file(), "no artifact at {}", artifact.display());
        artifact
    })
}

/// Records every crossing, rendered as a short string.
#[derive(Default)]
struct Recorder {
    events: Mutex<Vec<String>>,
}

impl Recorder {
    fn events(&self) -> Vec<String> {
        self.events.lock().unwrap().clone()
    }
}

impl TraceHook for Recorder {
    fn on_event(&self, event: &TraceEvent<'_>) {
        let rendered = match event {
            TraceEvent::ExportCall { func, .. } => format!("-> export {func}"),
            TraceEvent::ExportReturn { func, outcome, .. } => {
                format!("<- export {func} {}", describe(outcome))
            }
            TraceEvent::ImportCall {
                interface, func, ..
            } => format!("<- import {interface}#{func}"),
            TraceEvent::ImportReturn {
                interface, func, ..
            } => format!("-> import {interface}#{func}"),
        };
        self.events.lock().unwrap().push(rendered);
    }
}

fn describe(outcome: &Outcome<'_>) -> &'static str {
    match outcome {
        Outcome::Returned(_) => "ok",
        Outcome::Failed(_) => "failed",
    }
}

/// The policy a real Rust guest needs.
///
/// Worth reading closely: `clocks` and `env` are granted even though the linter
/// never asks for the time or reads a variable. The `wasm32-wasip2` target
/// links `std`, and `std` imports `wasi:clocks/monotonic-clock` and
/// `wasi:cli/environment` unconditionally. The import list reflects what the
/// *toolchain* linked, not what the author wrote — so a deny-by-default host
/// has to be told about them, or no ordinary Rust plugin loads at all.
const SAMPLE_POLICY: &str = r#"
[permissions]
clocks = "monotonic"
env    = {}

[limits]
fuel = 200_000_000
"#;

/// A host that serves `watoots:example/log`, collecting what the plugin logs.
fn host_serving_log(logged: Arc<Mutex<Vec<String>>>, hook: Option<Arc<dyn TraceHook>>) -> Host {
    let mut builder = Host::builder()
        .manifest(Manifest::parse(SAMPLE_POLICY).unwrap())
        .host_func("watoots:example/log@0.1.0", "emit", move |call| {
            let args = call.args();
            let level = match &args[0] {
                Val::Enum(name) => name.clone(),
                other => panic!("expected an enum for severity, got {other:?}"),
            };
            let message = match &args[1] {
                Val::String(text) => text.clone(),
                other => panic!("expected a string for message, got {other:?}"),
            };
            logged.lock().unwrap().push(format!("{level}: {message}"));
            Ok(vec![])
        });
    if let Some(hook) = hook {
        builder = builder.trace_hook(hook);
    }
    builder.build().unwrap()
}

#[test]
fn the_sample_plugin_reports_its_name() {
    let host = host_serving_log(Arc::new(Mutex::new(Vec::new())), None);
    let mut plugin = host.load(sample_plugin()).unwrap();

    assert_eq!(plugin.name(), "rust_lint");
    let results = plugin.call("name", &[]).unwrap();
    assert_eq!(results, vec![Val::String("rust-lint".to_string())]);
}

#[test]
fn the_sample_plugin_returns_typed_diagnostics() {
    let host = host_serving_log(Arc::new(Mutex::new(Vec::new())), None);
    let mut plugin = host.load(sample_plugin()).unwrap();

    let source = "fine\nTODO: fix this\ntrailing   \n";
    let results = plugin
        .call(
            "lint",
            &[
                Val::String("notes.md".to_string()),
                Val::String(source.to_string()),
            ],
        )
        .unwrap();

    let Some(Val::List(diagnostics)) = results.first() else {
        panic!("expected a list of diagnostics, got {results:?}");
    };
    assert_eq!(diagnostics.len(), 2, "{diagnostics:?}");

    // Records come back as named fields, not as a blob either side had to
    // agree on how to decode.
    let Val::Record(first) = &diagnostics[0] else {
        panic!("expected a record, got {:?}", diagnostics[0]);
    };
    let field = |name: &str| {
        first
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
            .unwrap_or_else(|| panic!("no field {name} in {first:?}"))
    };
    assert_eq!(field("line"), Val::U32(2));
    assert_eq!(field("severity"), Val::Enum("error".to_string()));
    assert_eq!(field("message"), Val::String("unresolved TODO".to_string()));
}

#[test]
fn the_plugin_calls_back_into_the_host() {
    let logged = Arc::new(Mutex::new(Vec::new()));
    let host = host_serving_log(Arc::clone(&logged), None);
    let mut plugin = host.load(sample_plugin()).unwrap();

    plugin
        .call(
            "lint",
            &[
                Val::String("a.md".to_string()),
                Val::String("TODO\n".to_string()),
            ],
        )
        .unwrap();

    let logged = logged.lock().unwrap().clone();
    assert_eq!(
        logged,
        ["hint: linting a.md", "hint: 1 diagnostic(s)"],
        "the host should have seen both log calls"
    );
}

#[test]
fn the_trace_hook_sees_both_directions() {
    let recorder = Arc::new(Recorder::default());
    let host = host_serving_log(
        Arc::new(Mutex::new(Vec::new())),
        Some(Arc::clone(&recorder) as Arc<dyn TraceHook>),
    );
    let mut plugin = host.load(sample_plugin()).unwrap();

    plugin
        .call(
            "lint",
            &[
                Val::String("a.md".to_string()),
                Val::String("TODO\n".to_string()),
            ],
        )
        .unwrap();

    // This ordering is what a replay has to reproduce: the export call, the
    // imports the guest made while inside it, then the export returning.
    //
    // The interface is recorded with its version. Grants match unversioned so a
    // rebuild does not invalidate a manifest, but a trace has to record what
    // actually crossed — replay matches against exactly this.
    assert_eq!(
        recorder.events(),
        [
            "-> export lint",
            "<- import watoots:example/log@0.1.0#emit",
            "-> import watoots:example/log@0.1.0#emit",
            "<- import watoots:example/log@0.1.0#emit",
            "-> import watoots:example/log@0.1.0#emit",
            "<- export lint ok",
        ]
    );
}

#[test]
fn an_unserved_host_interface_fails_to_instantiate() {
    // Declaring an interface satisfies the permission check but does not serve
    // it; the two must fail differently, and visibly.
    let host = Host::builder()
        .manifest(Manifest::parse(SAMPLE_POLICY).unwrap())
        .provide_interface("watoots:example/log")
        .build()
        .unwrap();

    let wasm = std::fs::read(sample_plugin()).unwrap();
    let report = host.inspect(&wasm).unwrap();
    assert!(report.is_satisfied(), "{}", report.describe());

    let err = host.load(sample_plugin()).unwrap_err();
    assert_eq!(err.kind(), watoots::ErrorKind::Load, "{}", err.message());
}

#[test]
fn a_registry_holds_several_plugins_over_one_engine() {
    let host = host_serving_log(Arc::new(Mutex::new(Vec::new())), None);
    let mut registry = Registry::new(host);

    let wasm = std::fs::read(sample_plugin()).unwrap();
    registry.load_binary("lint-a", &wasm).unwrap();
    registry.load_binary("lint-b", &wasm).unwrap();

    assert_eq!(registry.len(), 2);
    assert_eq!(registry.names().collect::<Vec<_>>(), ["lint-a", "lint-b"]);

    for name in ["lint-a", "lint-b"] {
        let results = registry.call(name, "name", &[]).unwrap();
        assert_eq!(results, vec![Val::String("rust-lint".to_string())]);
    }

    assert!(registry.remove("lint-a").is_some());
    assert_eq!(registry.len(), 1);
    assert_eq!(
        registry.call("lint-a", "name", &[]).unwrap_err().kind(),
        watoots::ErrorKind::NotFound
    );
}

#[test]
fn registering_the_same_name_twice_is_refused() {
    let host = host_serving_log(Arc::new(Mutex::new(Vec::new())), None);
    let mut registry = Registry::new(host);
    let wasm = std::fs::read(sample_plugin()).unwrap();

    registry.load_binary("lint", &wasm).unwrap();
    let err = registry.load_binary("lint", &wasm).unwrap_err();
    assert_eq!(err.kind(), watoots::ErrorKind::InvalidArgument);
}

#[test]
fn wave_text_goes_in_and_comes_back_out() {
    // The path a CLI or a C caller takes: strings in, strings out, with the
    // function's own type deciding how to read them.
    let host = host_serving_log(Arc::new(Mutex::new(Vec::new())), None);
    let mut plugin = host.load(sample_plugin()).unwrap();

    assert_eq!(plugin.call_wave("name", &[]).unwrap(), ["\"rust-lint\""]);

    let rendered = plugin
        .call_wave("lint", &["\"notes.md\"", "\"TODO: fix\\n\""])
        .unwrap();
    assert_eq!(rendered.len(), 1);
    assert!(
        rendered[0].contains("unresolved TODO"),
        "diagnostics should render as readable WAVE, got: {}",
        rendered[0]
    );
    assert!(rendered[0].contains("severity: error"), "{}", rendered[0]);
}

#[test]
fn call_wave_rejects_the_wrong_argument_count() {
    let host = host_serving_log(Arc::new(Mutex::new(Vec::new())), None);
    let mut plugin = host.load(sample_plugin()).unwrap();

    let err = plugin.call_wave("lint", &["\"only-one\""]).unwrap_err();
    assert_eq!(err.kind(), watoots::ErrorKind::InvalidArgument);
    assert!(err.message().contains("2 argument(s)"), "{}", err.message());
}
