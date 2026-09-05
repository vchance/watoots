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

    // The default view answers "what can it do", not "what does it import":
    // a capability row rather than an interface name.
    let text = stdout(&output);
    assert!(text.contains("capabilities"), "{text}");
    assert!(text.contains("clock"), "{text}");
    assert!(text.contains("DENY"), "{text}");
    // An interface the application is expected to serve is not a denial, and
    // must not be filed as one.
    assert!(text.contains("your application must serve"), "{text}");
    // Denials mean a non-zero exit, so `watoots inspect` works in a gate.
    assert!(!output.status.success());
}

#[test]
fn inspect_imports_lists_the_interfaces_individually() {
    let plugin = sample_plugin().display().to_string();
    let output = watoots(&["inspect", &plugin, "--imports"]);

    let text = stdout(&output);
    assert!(text.contains("wasi:clocks/monotonic-clock"), "{text}");
    assert!(text.contains("DENY"), "{text}");
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

#[test]
fn inspect_targets_reports_a_world_the_component_does_not_implement() {
    let plugin = sample_plugin().display().to_string();
    let wit = tempfile::Builder::new()
        .suffix(".wit")
        .tempfile()
        .expect("creating a WIT file");
    std::fs::write(
        wit.path(),
        "package test:other@0.1.0;\nworld formatter {\n  export format: func() -> string;\n}\n",
    )
    .expect("writing WIT");

    let output = watoots(&[
        "inspect",
        &plugin,
        "--targets",
        &wit.path().display().to_string(),
    ]);
    let text = stdout(&output);
    assert!(text.contains("does not implement world"), "{text}");
    assert!(!output.status.success());
}

#[test]
fn wit_semver_check_accepts_a_world_that_demands_less() {
    let dir = tempfile::tempdir().expect("temp dir");
    let previous = dir.path().join("previous.wit");
    let current = dir.path().join("current.wit");
    // The compatible direction: the new world asks plugins for fewer exports,
    // so everything built against the old one still satisfies it.
    std::fs::write(
        &previous,
        "package test:api@0.1.0;\nworld api {\n  export greet: func() -> string;\n  \
         export farewell: func() -> string;\n}\n",
    )
    .unwrap();
    std::fs::write(
        &current,
        "package test:api@0.2.0;\nworld api {\n  export greet: func() -> string;\n}\n",
    )
    .unwrap();

    let output = watoots(&[
        "wit",
        "semver-check",
        "--previous",
        &previous.display().to_string(),
        "--current",
        &current.display().to_string(),
    ]);
    assert!(output.status.success(), "{}", stdout(&output));
}

#[test]
fn wit_semver_check_rejects_a_changed_signature() {
    let dir = tempfile::tempdir().expect("temp dir");
    let previous = dir.path().join("previous.wit");
    let current = dir.path().join("current.wit");
    std::fs::write(
        &previous,
        "package test:api@0.1.0;\nworld api {\n  export greet: func() -> string;\n}\n",
    )
    .unwrap();
    std::fs::write(
        &current,
        "package test:api@0.2.0;\nworld api {\n  export greet: func(formal: bool) -> string;\n}\n",
    )
    .unwrap();

    let output = watoots(&[
        "wit",
        "semver-check",
        "--previous",
        &previous.display().to_string(),
        "--current",
        &current.display().to_string(),
    ]);
    assert!(!output.status.success(), "{}", stdout(&output));
    assert!(
        stdout(&output).contains("incompatible"),
        "{}",
        stdout(&output)
    );
}

// ---------------------------------------------------------------------------
// `watoots fuzz` — ADR-0008
// ---------------------------------------------------------------------------

/// Rolls a random number. The argument is ignored: it is there so the generator
/// has something to generate, and so the trace has an argument to encode.
///
/// WAT rather than a compiled guest, so this stays hermetic — `Component::new`
/// assembles the text itself, which is the same trick the host's own tests use.
/// `wasi:random` is the point: under the manifest's default determinism it is a
/// seeded generator and a session reproduces, and with determinism switched off
/// it is the real thing and a session does not. That makes it the smallest
/// honest demonstration of what the fuzzer is looking for.
const ROLLS: &str = r#"
(component
  (type (;0;)
    (instance
      (type (;0;) (func (result u64)))
      (export (;0;) "get-random-u64" (func (type 0)))
    )
  )
  (import "wasi:random/random@0.2.9" (instance $r (type 0)))
  (alias export $r "get-random-u64" (func $rand_fn))
  (core func $rand_lowered (canon lower (func $rand_fn)))
  (core module $m
    (import "r" "get-random-u64" (func $rand (result i64)))
    (func (export "roll") (param $n i32) (result i64) (call $rand))
  )
  (core instance $r_i (export "get-random-u64" (func $rand_lowered)))
  (core instance $i (instantiate $m (with "r" (instance $r_i))))
  (func $roll (param "n" u32) (result u64) (canon lift (core func $i "roll")))
  (export "roll" (func $roll))
)
"#;

/// Writes the component and a manifest into a temp dir, and returns both paths
/// plus somewhere to put crash files.
fn rolling_plugin(determinism: bool) -> (tempfile::TempDir, String, String, String) {
    let dir = tempfile::tempdir().expect("temp dir");
    let component = dir.path().join("rolls.wasm");
    let manifest = dir.path().join("policy.toml");
    let out = dir.path().join("findings");
    std::fs::write(&component, ROLLS).unwrap();
    std::fs::write(
        &manifest,
        if determinism {
            "[permissions]\nrandom = true\n"
        } else {
            "[permissions]\nrandom = true\n\n[determinism]\nenabled = false\n"
        },
    )
    .unwrap();
    (
        dir,
        component.display().to_string(),
        manifest.display().to_string(),
        out.display().to_string(),
    )
}

#[test]
fn fuzz_reports_nothing_when_a_session_reproduces() {
    let (_dir, component, manifest, out) = rolling_plugin(true);
    let output = watoots(&[
        "fuzz", &component, "-m", &manifest, "--cases", "8", "--out", &out,
    ]);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "{stderr}{}", stdout(&output));
    assert!(stderr.contains("no findings"), "{stderr}");
}

#[test]
fn fuzz_catches_a_session_that_does_not_reproduce() {
    // Determinism off, so the plugin answers differently on the replay than it
    // did on the recording. That is exactly the class of bug record/replay is
    // for, and the fuzzer has to notice it without being told what `roll`
    // should return.
    let (_dir, component, manifest, out) = rolling_plugin(false);
    let output = watoots(&[
        "fuzz", &component, "-m", &manifest, "--cases", "8", "--out", &out,
    ]);
    let text = stdout(&output);

    assert!(!output.status.success(), "{text}");
    assert!(text.contains("crash 0"), "{text}");
    assert!(text.contains("diverged"), "{text}");

    // The crash is an artifact, not just a message: a trace file, and the line
    // that turns it into a Rust regression test.
    let crash = std::path::Path::new(&out).join("crash-000.wave");
    assert!(crash.is_file(), "{text}");
    let recorded = std::fs::read_to_string(&crash).unwrap();
    assert!(recorded.starts_with("watoots-trace 1\n"), "{recorded}");
    assert!(recorded.contains("export-call roll"), "{recorded}");
    assert!(
        text.contains("watoots replay") && text.contains("--emit-test"),
        "a crash has to say how to become a test: {text}"
    );

    // And that file really is replayable input, not just something written out.
    let replayed = watoots(&[
        "replay",
        &crash.display().to_string(),
        "--component",
        &component,
        "--assert",
    ]);
    assert!(!replayed.status.success(), "{}", stdout(&replayed));
}

#[test]
fn fuzz_reproduces_a_case_from_its_seed() {
    // A crash report carries `--seed <n> --cases 1`. If that did not reproduce
    // the report would be useless, so the campaign has to be a function of the
    // seed and nothing else.
    let (_dir, component, manifest, out) = rolling_plugin(true);
    let run = || {
        let output = watoots(&[
            "fuzz", &component, "-m", &manifest, "--cases", "1", "--seed", "9137", "--out", &out,
        ]);
        assert!(output.status.success());
        // The trace itself is the thing that has to match, so record one.
        let trace = std::path::Path::new(&out).join("seed.wave");
        let recorded = watoots(&[
            "record",
            &component,
            "-m",
            &manifest,
            "-c",
            "roll",
            "-o",
            &trace.display().to_string(),
            "7",
        ]);
        assert!(recorded.status.success());
        std::fs::read_to_string(&trace).unwrap()
    };
    assert_eq!(run(), run());
}

#[test]
fn fuzz_refuses_an_export_the_component_does_not_have() {
    let (_dir, component, manifest, out) = rolling_plugin(true);
    let output = watoots(&[
        "fuzz", &component, "-m", &manifest, "--call", "nope", "--out", &out,
    ]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no function \"nope\""), "{stderr}");
    assert!(
        stderr.contains("roll"),
        "a refusal should say what there is: {stderr}"
    );
}

#[test]
fn profile_reports_the_three_buckets_and_names_the_functions() {
    let plugin = sample_plugin().display().to_string();
    let output = watoots(&[
        "profile",
        &plugin,
        "-m",
        &policy(),
        "--answer",
        "watoots:example/log@0.1.0#emit=",
        "-c",
        "lint",
        "--repeat",
        "5",
        "--",
        r#""notes.md""#,
        r#""TODO: fix\n""#,
    ]);
    assert!(output.status.success(), "{output:?}");

    let text = stdout(&output);
    assert!(text.contains("5 call(s)"), "{text}");
    assert!(text.contains("guest"), "{text}");
    assert!(text.contains("host call"), "{text}");
    assert!(text.contains("marshalling"), "{text}");
    // The number most likely to be over-read says what it is, every time.
    assert!(text.contains("marshalling is the remainder"), "{text}");
    // Per-function attribution: the export by name, and the host function the
    // guest called by interface and name.
    assert!(text.contains("export  lint"), "{text}");
    assert!(
        text.contains("import  watoots:example/log@0.1.0#emit"),
        "{text}"
    );
}

#[test]
fn profile_writes_a_firefox_profile_when_asked() {
    let dir = tempfile::tempdir().unwrap();
    let json = dir.path().join("guest.json");
    let plugin = sample_plugin().display().to_string();

    let output = watoots(&[
        "profile",
        &plugin,
        "-m",
        &policy(),
        "--answer",
        "watoots:example/log@0.1.0#emit=",
        "-c",
        "lint",
        "--firefox",
        &json.display().to_string(),
        "--",
        r#""notes.md""#,
        r#""TODO: fix\n""#,
    ]);
    assert!(output.status.success(), "{output:?}");

    let written = std::fs::read_to_string(&json).unwrap();
    assert!(written.starts_with('{'), "the processed profile is JSON");
    assert!(written.contains("\"threads\""), "and it has threads");
}

#[test]
fn profiling_and_recording_are_separate_subcommands() {
    // ADR-0009 refuses the combination rather than permitting it silently, and
    // the CLI expresses that by not offering a --record flag on `profile`.
    let plugin = sample_plugin().display().to_string();
    let output = watoots(&["profile", &plugin, "-c", "lint", "--output", "t.wave"]);
    assert!(!output.status.success());
    let text = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(text.contains("unexpected argument"), "{text}");
}

// ---------------------------------------------------------------------------
// The asset plugin: the sample that needs a capability.
// ---------------------------------------------------------------------------

/// Build `examples/plugins/rust-asset` and install it beside its `luts/`.
///
/// Returned relative to the repository root, which is the directory `watoots()`
/// runs in — and it has to be, because `${plugin_dir}` in the shipped policy
/// expands to the component path *as spelled on the command line*, and watoots
/// preopens a granted directory under exactly the name it was granted as. A
/// component named through `target/` would preopen a `luts/` that is not there.
fn asset_plugin() -> &'static str {
    static ARTIFACT: OnceLock<()> = OnceLock::new();
    ARTIFACT.get_or_init(|| {
        let root = repo_root();
        let crate_dir = root.join("examples/plugins/rust-asset");
        let status = Command::new(env!("CARGO"))
            .args(["build", "--manifest-path"])
            .arg(crate_dir.join("Cargo.toml"))
            .args(["--target", "wasm32-wasip2", "--release"])
            .status()
            .expect("running cargo to build the asset plugin");
        assert!(status.success(), "failed to build the asset plugin");
        std::fs::copy(
            crate_dir.join("target/wasm32-wasip2/release/rust_asset.wasm"),
            crate_dir.join("rust_asset.wasm"),
        )
        .expect("installing the artifact beside its luts/");
    });
    "examples/plugins/rust-asset/rust_asset.wasm"
}

fn asset_policy() -> &'static str {
    "examples/policies/rust-asset.toml"
}

/// A 2x2 image as WAVE: a mid-tone, a saturated pixel, white and black.
const ASSET_IMAGE: &str =
    "{width: 2, height: 2, pixels: [10, 20, 30, 200, 100, 50, 255, 255, 255, 0, 0, 0]}";

#[test]
fn inspect_reports_the_asset_plugin_as_wanting_exactly_one_directory() {
    let output = watoots(&[
        "inspect",
        asset_plugin(),
        "-m",
        asset_policy(),
        "--provide",
        "watoots:asset/log",
    ]);

    let text = stdout(&output);
    // The install-time answer to "what does this thing want": one directory,
    // named, and nothing else beyond what `std` drags in.
    assert!(text.contains("reads ${plugin_dir}/luts"), "{text}");
    assert!(text.contains("network      -"), "{text}");
    assert!(text.contains("every import is granted"), "{text}");
    assert!(output.status.success(), "{output:?}");
}

#[test]
fn describe_lists_bare_operation_kinds() {
    // `supports` is `list<operation-kind>`, so this renders as five enum cases
    // and nothing else. When it was `list<operation>` the same call printed
    // `gain({channel: red, factor: 1})` — a record of invented values a reader
    // had to know to ignore, and a shape four guests would have invented four
    // different ways.
    let output = watoots(&[
        "run",
        asset_plugin(),
        "-m",
        asset_policy(),
        "--answer",
        "watoots:asset/log@0.1.0#emit=",
        "-c",
        "describe",
    ]);

    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        stdout(&output).trim(),
        r#"{name: "rust-asset", supports: [grayscale, invert, gain, resize, lut]}"#
    );
}

#[test]
fn the_asset_plugin_does_not_load_without_the_filesystem_grant() {
    // The same policy minus one line. `lut` never runs; the component declares
    // `wasi:filesystem` in its binary, so this is a load error and the CLI
    // exits non-zero with the interface named.
    let dir = tempfile::tempdir().unwrap();
    let policy = dir.path().join("no-fs.toml");
    let text = std::fs::read_to_string(repo_root().join(asset_policy())).unwrap();
    let stripped: String = text
        .lines()
        .filter(|line| !line.starts_with("fs.read"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&policy, stripped).unwrap();

    let output = watoots(&[
        "run",
        asset_plugin(),
        "-m",
        &policy.display().to_string(),
        "--answer",
        "watoots:asset/log@0.1.0#emit=",
        "-c",
        "apply",
        "--",
        ASSET_IMAGE,
        "[invert]",
    ]);

    assert!(!output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("wasi:filesystem"), "{stderr}");
}

#[test]
fn the_asset_plugin_reads_its_lookup_table_through_the_grant() {
    // With the grant, the same plugin opens the table itself and the pipeline
    // completes. The pixels are the hand-computed ones from
    // `crates/host/tests/asset_e2e.rs`; this is the command-line spelling of
    // the same claim, with WAVE on both sides.
    let output = watoots(&[
        "run",
        asset_plugin(),
        "-m",
        asset_policy(),
        "--answer",
        "watoots:asset/log@0.1.0#emit=",
        "-c",
        "apply",
        "--",
        ASSET_IMAGE,
        r#"[grayscale, lut("examples/plugins/rust-asset/luts/sepia.lut")]"#,
    ]);

    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        stdout(&output).trim(),
        "ok({width: 2, height: 2, pixels: [24, 22, 17, 168, 149, 116, 255, 255, 239, 0, 0, 0]})"
    );
}

#[test]
fn a_lut_the_grant_does_not_cover_is_an_answer_and_not_a_crash() {
    // A path outside the preopened directory. WASI refuses it, the plugin turns
    // that into `unreadable`, and `run` exits zero because the call succeeded —
    // the *plugin* said no, which is a different thing from the host saying no.
    let output = watoots(&[
        "run",
        asset_plugin(),
        "-m",
        asset_policy(),
        "--answer",
        "watoots:asset/log@0.1.0#emit=",
        "-c",
        "apply",
        "--",
        ASSET_IMAGE,
        r#"[lut("examples/policies/rust-asset.toml")]"#,
    ]);

    assert!(output.status.success(), "{output:?}");
    // `unreadable` carries `file-failure { path, reason }`, so the reason is in
    // the value the caller already has. Nobody has to go and find a log sink to
    // learn that the manifest is the thing to look at.
    let text = stdout(&output);
    assert!(
        text.starts_with(r#"err(unreadable({path: "examples/policies/rust-asset.toml", reason: "#),
        "{text}"
    );
    assert!(text.contains("check the manifest"), "{text}");
}

#[test]
fn a_malformed_image_comes_back_as_a_failure_rather_than_a_trap() {
    // 2x2 needs 12 bytes of RGB8; this is 11.
    let output = watoots(&[
        "run",
        asset_plugin(),
        "-m",
        asset_policy(),
        "--answer",
        "watoots:asset/log@0.1.0#emit=",
        "-c",
        "apply",
        "--",
        "{width: 2, height: 2, pixels: [10, 20, 30, 200, 100, 50, 255, 255, 255, 0, 0]}",
        "[invert]",
    ]);

    assert!(output.status.success(), "{output:?}");
    let text = stdout(&output);
    assert!(text.starts_with("err(malformed("), "{text}");
    assert!(text.contains("12 bytes"), "{text}");
    assert!(text.contains("got 11"), "{text}");
}
