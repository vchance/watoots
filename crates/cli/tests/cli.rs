//! The `watoots` command line, driven as a user would.

use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::OnceLock;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn sample_plugin() -> &'static PathBuf {
    static ARTIFACT: OnceLock<PathBuf> = OnceLock::new();
    ARTIFACT.get_or_init(|| {
        let root = repo_root();
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

fn watoots(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_watoots"))
        .args(args)
        .current_dir(repo_root())
        .output()
        .expect("running watoots")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn policy() -> String {
    repo_root()
        .join("examples/policies/rust-lint.toml")
        .display()
        .to_string()
}

#[test]
fn inspect_without_a_manifest_shows_the_whole_bill() {
    let plugin = sample_plugin().display().to_string();
    let output = watoots(&["inspect", &plugin]);

    let text = stdout(&output);
    assert!(text.contains("wasi:clocks/monotonic-clock"), "{text}");
    assert!(text.contains("DENY"), "{text}");
    // Denials mean a non-zero exit, so `watoots inspect` works in a gate.
    assert!(!output.status.success());
}

#[test]
fn inspect_with_a_policy_and_the_served_interface_is_clean() {
    let plugin = sample_plugin().display().to_string();
    let output = watoots(&[
        "inspect",
        &plugin,
        "-m",
        &policy(),
        "--provide",
        "watoots:example/log",
    ]);

    let text = stdout(&output);
    assert!(text.contains("every import is granted"), "{text}");
    assert!(output.status.success());
}

#[test]
fn record_then_replay_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let trace = dir.path().join("lint.wave");
    let plugin = sample_plugin().display().to_string();

    let recorded = watoots(&[
        "record",
        &plugin,
        "-m",
        &policy(),
        "--answer",
        "watoots:example/log@0.1.0#emit=",
        "-c",
        "lint",
        "-o",
        &trace.display().to_string(),
        "--",
        r#""notes.md""#,
        r#""TODO: fix\n""#,
    ]);
    assert!(recorded.status.success(), "{:?}", recorded);

    // The trace is text, and readable. That is the whole differentiator.
    let text = std::fs::read_to_string(&trace).unwrap();
    assert!(text.starts_with("watoots-trace 1\n"), "{text}");
    assert!(text.contains("export-call lint"), "{text}");
    assert!(text.contains("unresolved TODO"), "{text}");

    let replayed = watoots(&[
        "replay",
        &trace.display().to_string(),
        "-c",
        &plugin,
        "--assert",
    ]);
    assert!(replayed.status.success(), "{}", stdout(&replayed));
    assert!(stdout(&replayed).contains("matched the trace"));
}

#[test]
fn an_edited_trace_fails_the_assert() {
    let dir = tempfile::tempdir().unwrap();
    let trace = dir.path().join("lint.wave");
    let plugin = sample_plugin().display().to_string();

    watoots(&[
        "record",
        &plugin,
        "-m",
        &policy(),
        "--answer",
        "watoots:example/log@0.1.0#emit=",
        "-c",
        "lint",
        "-o",
        &trace.display().to_string(),
        "--",
        r#""notes.md""#,
        r#""TODO\n""#,
    ]);

    // Hand-edit the recording, which is a thing you can do precisely because
    // it is text.
    let text = std::fs::read_to_string(&trace).unwrap();
    std::fs::write(&trace, text.replace("1 diagnostic(s)", "7 diagnostic(s)")).unwrap();

    let replayed = watoots(&[
        "replay",
        &trace.display().to_string(),
        "-c",
        &plugin,
        "--assert",
    ]);
    assert!(!replayed.status.success(), "the edit should have failed CI");

    let report = stdout(&replayed);
    assert!(report.contains("diverged"), "{report}");
    assert!(report.contains("7 diagnostic(s)"), "{report}");
    assert!(report.contains("1 diagnostic(s)"), "{report}");
}

#[test]
fn trace_fmt_converts_between_encodings() {
    let dir = tempfile::tempdir().unwrap();
    let text_trace = dir.path().join("lint.wave");
    let binary_trace = dir.path().join("lint.wtr");
    let plugin = sample_plugin().display().to_string();

    watoots(&[
        "record",
        &plugin,
        "-m",
        &policy(),
        "--answer",
        "watoots:example/log@0.1.0#emit=",
        "-c",
        "name",
        "-o",
        &text_trace.display().to_string(),
    ]);

    let converted = watoots(&[
        "trace",
        "fmt",
        &text_trace.display().to_string(),
        "--binary",
        "-o",
        &binary_trace.display().to_string(),
    ]);
    assert!(converted.status.success());
    assert!(std::fs::read(&binary_trace).unwrap().starts_with(b"WTTR"));

    // And back again, unchanged.
    let back = watoots(&["trace", "fmt", &binary_trace.display().to_string()]);
    assert_eq!(stdout(&back), std::fs::read_to_string(&text_trace).unwrap());
}

#[test]
fn replaying_against_the_wrong_component_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let trace = dir.path().join("lint.wave");
    let plugin = sample_plugin().display().to_string();
    let other = dir.path().join("other.wasm");
    std::fs::write(&other, b"(component)").unwrap();

    watoots(&[
        "record",
        &plugin,
        "-m",
        &policy(),
        "--answer",
        "watoots:example/log@0.1.0#emit=",
        "-c",
        "name",
        "-o",
        &trace.display().to_string(),
    ]);

    let output = watoots(&[
        "replay",
        &trace.display().to_string(),
        "-c",
        &other.display().to_string(),
    ]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("different component"),
        "{:?}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn emit_test_writes_a_runnable_fixture() {
    let dir = tempfile::tempdir().unwrap();
    let trace = dir.path().join("lint.wave");
    let fixture = dir.path().join("replay_test.rs");
    let plugin = sample_plugin().display().to_string();

    watoots(&[
        "record",
        &plugin,
        "-m",
        &policy(),
        "--answer",
        "watoots:example/log@0.1.0#emit=",
        "-c",
        "name",
        "-o",
        &trace.display().to_string(),
    ]);
    watoots(&[
        "replay",
        &trace.display().to_string(),
        "-c",
        &plugin,
        "--emit-test",
        &fixture.display().to_string(),
    ]);

    let source = std::fs::read_to_string(&fixture).unwrap();
    assert!(source.contains("#[test]"), "{source}");
    assert!(source.contains("watoots_trace::replay"), "{source}");
    assert!(source.contains("is_faithful"), "{source}");
}
