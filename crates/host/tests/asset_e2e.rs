//! The Rust asset plugin, end to end — and the capability it needs.
//!
//! `plugin_e2e.rs` proves the typed boundary works with a plugin that needs
//! nothing. This one is the other half: `examples/plugins/rust-asset` opens a
//! file itself, so it is the sample that shows a grant being required, scoped,
//! and — the part that matters — enforced at *load*, before any guest code runs.
//!
//! It also pins the arithmetic. Three more guest languages are meant to
//! implement this same world byte for byte, and "byte for byte" is only a claim
//! until something asserts the bytes. Every expected value below is computed by
//! hand from the rule documented in `examples/plugins/rust-asset/src/lib.rs`,
//! not captured from a run, so a future guest that disagrees fails here rather
//! than being blessed by whatever it happened to produce.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};

use watoots::{ErrorKind, Host, Manifest, Val};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Build the asset plugin once per test binary and return the artifact path.
///
/// The artifact is copied next to its `luts/` directory, exactly as
/// `tools/build-plugins.sh` does, because `${plugin_dir}` in the shipped policy
/// expands to the directory a component was *loaded from*. Loading straight out
/// of `target/` would make the policy under test a different policy.
fn asset_plugin() -> &'static PathBuf {
    static ARTIFACT: OnceLock<PathBuf> = OnceLock::new();
    ARTIFACT.get_or_init(|| {
        let root = repo_root();
        let crate_dir = root.join("examples/plugins/rust-asset");

        let status = Command::new(env!("CARGO"))
            .args(["build", "--manifest-path"])
            .arg(crate_dir.join("Cargo.toml"))
            .args(["--target", "wasm32-wasip2", "--release"])
            .status()
            .expect("running cargo to build the asset plugin");
        assert!(
            status.success(),
            "failed to build the asset plugin; is the wasm32-wasip2 target installed?"
        );

        let built = crate_dir.join("target/wasm32-wasip2/release/rust_asset.wasm");
        assert!(built.is_file(), "no artifact at {}", built.display());
        let installed = crate_dir.join("rust_asset.wasm");
        std::fs::copy(&built, &installed).expect("installing the artifact beside its luts/");
        installed
    })
}

/// The policy that ships with the example, read from disk rather than inlined.
///
/// If `examples/policies/rust-asset.toml` stops granting what the guest needs,
/// this suite is where that shows up — which is the only way a shipped policy
/// stays true.
fn shipped_policy() -> Manifest {
    Manifest::from_file(repo_root().join("examples/policies/rust-asset.toml"))
        .expect("reading examples/policies/rust-asset.toml")
}

/// The same policy with the filesystem grant taken away, and nothing else.
///
/// One difference from the shipped one, so a failure cannot be blamed on
/// anything but the missing grant.
fn policy_without_filesystem() -> Manifest {
    let mut manifest = shipped_policy();
    manifest.permissions.fs.read.clear();
    manifest.permissions.fs.write.clear();
    manifest
}

/// A host that serves `watoots:asset/log`, collecting what the plugin logs.
fn host_with(manifest: Manifest, logged: Arc<Mutex<Vec<String>>>) -> Host {
    Host::builder()
        .manifest(manifest)
        .host_func("watoots:asset/log@0.1.0", "emit", move |call| {
            let args = call.args();
            let level = match &args[0] {
                Val::Enum(name) => name.clone(),
                other => panic!("expected an enum for level, got {other:?}"),
            };
            let message = match &args[1] {
                Val::String(text) => text.clone(),
                other => panic!("expected a string for message, got {other:?}"),
            };
            logged.lock().unwrap().push(format!("{level}: {message}"));
            Ok(vec![])
        })
        .build()
        .unwrap()
}

fn quiet_host(manifest: Manifest) -> Host {
    host_with(manifest, Arc::new(Mutex::new(Vec::new())))
}

// ---------------------------------------------------------------------------
// Building `Val`s for the asset world.
// ---------------------------------------------------------------------------

fn image(width: u32, height: u32, pixels: &[u8]) -> Val {
    Val::Record(vec![
        ("width".to_string(), Val::U32(width)),
        ("height".to_string(), Val::U32(height)),
        (
            "pixels".to_string(),
            Val::List(pixels.iter().copied().map(Val::U8).collect()),
        ),
    ])
}

fn step(name: &str) -> Val {
    Val::Variant(name.to_string(), None)
}

fn gain_step(channel: &str, factor: f32) -> Val {
    Val::Variant(
        "gain".to_string(),
        Some(Box::new(Val::Record(vec![
            ("channel".to_string(), Val::Enum(channel.to_string())),
            ("factor".to_string(), Val::Float32(factor)),
        ]))),
    )
}

fn resize_step(width: u32, height: u32) -> Val {
    Val::Variant(
        "resize".to_string(),
        Some(Box::new(Val::Record(vec![
            ("width".to_string(), Val::U32(width)),
            ("height".to_string(), Val::U32(height)),
        ]))),
    )
}

fn lut_step(name: &str) -> Val {
    Val::Variant("lut".to_string(), Some(Box::new(Val::String(name.into()))))
}

/// The `ok` arm of `result<image, failure>`, as `(width, height, pixels)`.
fn expect_ok(results: &[Val]) -> (u32, u32, Vec<u8>) {
    let Some(Val::Result(Ok(Some(value)))) = results.first() else {
        panic!("expected ok(image), got {results:?}");
    };
    let Val::Record(fields) = value.as_ref() else {
        panic!("expected an image record, got {value:?}");
    };
    let field = |name: &str| {
        fields
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
            .unwrap_or_else(|| panic!("no field {name} in {fields:?}"))
    };
    let (Val::U32(width), Val::U32(height), Val::List(pixels)) =
        (field("width"), field("height"), field("pixels"))
    else {
        panic!("unexpected field types in {fields:?}");
    };
    let pixels = pixels
        .iter()
        .map(|byte| match byte {
            Val::U8(value) => *value,
            other => panic!("expected u8 pixels, got {other:?}"),
        })
        .collect();
    (width, height, pixels)
}

/// The `err` arm of `result<image, failure>`, as `(case, payload)`.
fn expect_err(results: &[Val]) -> (String, Val) {
    let Some(Val::Result(Err(Some(value)))) = results.first() else {
        panic!("expected err(failure), got {results:?}");
    };
    let Val::Variant(case, Some(payload)) = value.as_ref() else {
        panic!("expected a failure variant, got {value:?}");
    };
    (case.clone(), payload.as_ref().clone())
}

/// The `string` payload of `unsupported` or `malformed`.
fn expect_err_text(results: &[Val]) -> (String, String) {
    let (case, payload) = expect_err(results);
    let Val::String(text) = payload else {
        panic!("expected a string payload, got {payload:?}");
    };
    (case, text)
}

/// The `file-failure` payload of `unreadable`, as `(path, reason)`.
///
/// The reason is the point of the record. Before it existed the only
/// machine-readable signal was a path, and a caller with no log sink could not
/// tell "the manifest does not cover this" from "that file is not a table".
fn expect_unreadable(results: &[Val]) -> (String, String) {
    let (case, payload) = expect_err(results);
    assert_eq!(case, "unreadable", "{payload:?}");
    let Val::Record(fields) = payload else {
        panic!("expected a file-failure record, got {payload:?}");
    };
    let field = |name: &str| match fields.iter().find(|(key, _)| key == name) {
        Some((_, Val::String(text))) => text.clone(),
        other => panic!("no string field {name}, got {other:?}"),
    };
    (field("path"), field("reason"))
}

/// A 2x2 image whose four pixels exercise different rounding.
///
/// Mid-tone, saturated, white and black: enough that a `+ 0.5` that is not
/// there, or a truncation that should have been a round, shows up.
const SOURCE: [u8; 12] = [
    10, 20, 30, // (0,0)
    200, 100, 50, // (1,0)
    255, 255, 255, // (0,1)
    0, 0, 0, // (1,1)
];

/// `SOURCE` after `grayscale`, by hand:
///
/// ```text
/// (10,20,30)     -> (2990 + 11740 + 3420 + 500) / 1000 = 18650 / 1000 =  18
/// (200,100,50)   -> (59800 + 58700 + 5700 + 500) / 1000 = 124700 / 1000 = 124
/// (255,255,255)  -> (255000 + 500) / 1000 = 255500 / 1000 = 255
/// (0,0,0)        -> 500 / 1000 = 0
/// ```
const GRAY: [u8; 12] = [18, 18, 18, 124, 124, 124, 255, 255, 255, 0, 0, 0];

fn lut_path() -> String {
    repo_root()
        .join("examples/plugins/rust-asset/luts/sepia.lut")
        .display()
        .to_string()
}

// ---------------------------------------------------------------------------

#[test]
fn describe_names_the_plugin_and_every_step_it_implements() {
    let host = quiet_host(shipped_policy());
    let mut plugin = host.load(asset_plugin()).unwrap();

    let results = plugin.call("describe", &[]).unwrap();
    let Some(Val::Record(fields)) = results.first() else {
        panic!("expected a plugin-info record, got {results:?}");
    };
    let supports = fields
        .iter()
        .find(|(key, _)| key == "supports")
        .map(|(_, value)| value.clone())
        .expect("a supports field");
    let Val::List(kinds) = supports else {
        panic!("expected a list of operation kinds, got {supports:?}");
    };

    // `list<operation-kind>`, so these are bare enum cases and there is nothing
    // else in them. Asserting the whole list is the assertion — when this was
    // `list<operation>` the same check had to reach past invented payloads that
    // a host was expected to know to ignore.
    assert_eq!(
        kinds,
        ["grayscale", "invert", "gain", "resize", "lut"]
            .map(|case| Val::Enum(case.to_string()))
            .to_vec()
    );
}

#[test]
fn grayscale_is_rec_601_luma_in_fixed_point() {
    // The single most copied rule in this world, so it gets its own assertion
    // with the arithmetic spelled out beside `GRAY`. A guest that reaches for
    // `0.299f * r + ...` instead disagrees here, on the mid-tone pixel.
    let host = quiet_host(shipped_policy());
    let mut plugin = host.load(asset_plugin()).unwrap();

    let results = plugin
        .call(
            "apply",
            &[image(2, 2, &SOURCE), Val::List(vec![step("grayscale")])],
        )
        .unwrap();
    assert_eq!(expect_ok(&results), (2, 2, GRAY.to_vec()));
}

#[test]
fn a_pipeline_applies_its_steps_in_order() {
    let host = quiet_host(shipped_policy());
    let mut plugin = host.load(asset_plugin()).unwrap();

    let results = plugin
        .call(
            "apply",
            &[
                image(2, 2, &SOURCE),
                Val::List(vec![
                    step("grayscale"),
                    step("invert"),
                    gain_step("red", 0.5),
                ]),
            ],
        )
        .unwrap();

    // grayscale: GRAY, above.
    // invert:    255 - c            -> 237, 131, 0, 255 in every channel.
    // gain red:  floor(c*0.5 + 0.5) -> 237 -> 119, 131 -> 66, 0 -> 0, 255 -> 128.
    #[rustfmt::skip]
    let expected = [
        119, 237, 237,
         66, 131, 131,
          0,   0,   0,
        128, 255, 255,
    ];
    assert_eq!(expect_ok(&results), (2, 2, expected.to_vec()));
}

#[test]
fn order_matters_and_the_test_can_tell() {
    // The same two steps the other way round. If `apply` ever ran steps in
    // some other order — or folded them — this is what would catch it.
    let host = quiet_host(shipped_policy());
    let mut plugin = host.load(asset_plugin()).unwrap();

    let inverted_then_gray = plugin
        .call(
            "apply",
            &[
                image(2, 2, &SOURCE),
                Val::List(vec![step("invert"), step("grayscale")]),
            ],
        )
        .unwrap();

    // invert first: (245,235,225), (55,155,205), (0,0,0), (255,255,255)
    //   (245,235,225) -> (73255 + 137945 + 25650 + 500) / 1000 = 237
    //   (55,155,205)  -> (16445 + 90985 + 23370 + 500) / 1000  = 131
    #[rustfmt::skip]
    let expected = [
        237, 237, 237,
        131, 131, 131,
          0,   0,   0,
        255, 255, 255,
    ];
    assert_eq!(expect_ok(&inverted_then_gray), (2, 2, expected.to_vec()));
}

#[test]
fn resize_is_nearest_neighbour_and_biased_to_the_top_left() {
    let host = quiet_host(shipped_policy());
    let mut plugin = host.load(asset_plugin()).unwrap();

    let results = plugin
        .call(
            "apply",
            &[image(2, 2, &SOURCE), Val::List(vec![resize_step(3, 3)])],
        )
        .unwrap();

    // sx = dx * 2 / 3 -> 0, 0, 1;  sy likewise. A centre-sampling filter would
    // give 0, 1, 1 instead, so this asserts the choice and not just the size.
    #[rustfmt::skip]
    let expected = [
        10, 20, 30,   10, 20, 30,   200, 100, 50,
        10, 20, 30,   10, 20, 30,   200, 100, 50,
        255, 255, 255, 255, 255, 255, 0, 0, 0,
    ];
    assert_eq!(expect_ok(&results), (3, 3, expected.to_vec()));
}

#[test]
fn a_lut_step_reads_its_table_through_the_granted_directory() {
    let logged = Arc::new(Mutex::new(Vec::new()));
    let host = host_with(shipped_policy(), Arc::clone(&logged));
    let mut plugin = host.load(asset_plugin()).unwrap();

    let results = plugin
        .call(
            "apply",
            &[
                image(2, 2, &SOURCE),
                Val::List(vec![step("grayscale"), lut_step(&lut_path())]),
            ],
        )
        .unwrap();

    // GRAY is 18, 124, 255, 0 in every channel, and `luts/sepia.lut` maps
    // those to (24,22,17), (168,149,116), (255,255,239) and (0,0,0). The
    // table is applied per channel: the red output comes from the red column
    // of the entry the red input selects, and never from green or blue.
    #[rustfmt::skip]
    let expected = [
         24,  22,  17,
        168, 149, 116,
        255, 255, 239,
          0,   0,   0,
    ];
    assert_eq!(expect_ok(&results), (2, 2, expected.to_vec()));

    // Nothing was logged at error: the read succeeded.
    let logged = logged.lock().unwrap().clone();
    assert_eq!(logged, ["info: apply: 2x2, 2 step(s)"], "{logged:?}");
}

#[test]
fn without_the_filesystem_grant_the_plugin_does_not_load_at_all() {
    // The point of the example. `lut` never runs, no pixel is touched, and the
    // failure is not a runtime denial deep inside a pipeline: the component
    // *declares* `wasi:filesystem` in its binary, so the mismatch is visible
    // before instantiation.
    let host = quiet_host(policy_without_filesystem());

    let err = host.load(asset_plugin()).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::PermissionDenied, "{}", err.message());
    assert!(
        err.message().contains("wasi:filesystem"),
        "the denial should name the interface: {}",
        err.message()
    );

    // And `inspect` says the same thing without loading anything, which is the
    // install-time version of this check.
    let wasm = std::fs::read(asset_plugin()).unwrap();
    let report = host.inspect(&wasm).unwrap();
    assert!(!report.is_satisfied(), "{}", report.describe());
}

#[test]
fn a_lut_outside_the_granted_directory_is_unreadable_rather_than_fatal() {
    // The grant is scoped to `luts/`, so a table beside it is refused by WASI
    // — and the plugin turns that into an answer rather than a trap.
    let logged = Arc::new(Mutex::new(Vec::new()));
    let host = host_with(shipped_policy(), Arc::clone(&logged));
    let mut plugin = host.load(asset_plugin()).unwrap();

    let outside = repo_root()
        .join("examples/policies/rust-asset.toml")
        .display()
        .to_string();
    let results = plugin
        .call(
            "apply",
            &[image(2, 2, &SOURCE), Val::List(vec![lut_step(&outside)])],
        )
        .unwrap();

    let (path, reason) = expect_unreadable(&results);
    assert_eq!(path, outside, "the path is the one that was asked for");

    // The reason travels in the return value, so a caller with no log sink
    // still learns why. It also says the thing that is actually true here:
    // WASI reports a path outside every preopen as *not found*, so the plugin
    // cannot distinguish "denied" from "missing" and does not pretend to.
    assert!(reason.contains("cannot open it"), "{reason}");
    assert!(reason.contains("check the manifest"), "{reason}");

    // The log line is now a courtesy rather than the only channel, and there
    // is exactly one of it.
    let logged = logged.lock().unwrap().clone();
    assert_eq!(logged.len(), 2, "one info, one error: {logged:?}");
    assert!(
        logged[1].starts_with("error: unreadable lookup table"),
        "{logged:?}"
    );
}

/// A file that exists for the duration of one test and is removed even if the
/// test panics.
///
/// It has to live *inside* the granted directory to be reachable at all, and
/// that directory is in the working tree — so leaving one behind would show up
/// as an untracked file in someone's `git status`.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[test]
fn a_corrupt_table_is_reported_rather_than_silently_clipped() {
    // A file inside the grant that is not a lookup table. Reading it succeeds
    // and parsing it must not.
    let scratch =
        Scratch(repo_root().join("examples/plugins/rust-asset/luts/scratch-not-a-table.lut"));
    std::fs::write(&scratch.0, "0 0 0\n1 1 1\n").unwrap();

    let logged = Arc::new(Mutex::new(Vec::new()));
    let host = host_with(shipped_policy(), Arc::clone(&logged));
    let mut plugin = host.load(asset_plugin()).unwrap();

    let results = plugin
        .call(
            "apply",
            &[
                image(2, 2, &SOURCE),
                Val::List(vec![lut_step(&scratch.0.display().to_string())]),
            ],
        )
        .unwrap();

    // Same case as a missing file, different reason — which is exactly what
    // `file-failure` was added to make possible. A caller that only saw
    // `unreadable` here would go and check the manifest for a file the
    // manifest already covers.
    let (path, reason) = expect_unreadable(&results);
    assert_eq!(path, scratch.0.display().to_string());
    assert_eq!(reason, "expected 256 entries, found 2");

    // And the other end: a table with an entry too many is refused at the
    // 257th rather than read to the end. A grant is a directory, so "what is
    // in luts/" is not a closed set, and neither direction may be tolerated
    // quietly — a short table would clip highlights and a long one would say
    // nothing about which 256 entries won.
    let long = Scratch(repo_root().join("examples/plugins/rust-asset/luts/scratch-too-long.lut"));
    let mut text = String::new();
    for _ in 0..257 {
        text.push_str("0 0 0\n");
    }
    std::fs::write(&long.0, text).unwrap();
    logged.lock().unwrap().clear();

    let results = plugin
        .call(
            "apply",
            &[
                image(2, 2, &SOURCE),
                Val::List(vec![lut_step(&long.0.display().to_string())]),
            ],
        )
        .unwrap();

    let (_, reason) = expect_unreadable(&results);
    assert_eq!(reason, "more than 256 entries: a 257th appears on line 257");
}

#[test]
fn a_malformed_image_is_a_failure_and_not_a_trap() {
    let host = quiet_host(shipped_policy());
    let mut plugin = host.load(asset_plugin()).unwrap();

    // 2x2 needs 12 bytes; this is 11. The distinction that matters is that
    // `call` returns `Ok` — the guest answered — and the answer is `err`.
    let results = plugin
        .call(
            "apply",
            &[image(2, 2, &SOURCE[..11]), Val::List(vec![step("invert")])],
        )
        .unwrap();

    let (case, payload) = expect_err_text(&results);
    assert_eq!(case, "malformed");
    assert!(payload.contains("12 bytes"), "{payload}");
    assert!(payload.contains("got 11"), "{payload}");

    // The plugin is still usable afterwards, which a trap would not leave true.
    let ok = plugin
        .call("apply", &[image(2, 2, &SOURCE), Val::List(vec![])])
        .unwrap();
    assert_eq!(expect_ok(&ok), (2, 2, SOURCE.to_vec()));
}

#[test]
fn an_absurd_resize_is_refused_instead_of_exhausting_memory() {
    let host = quiet_host(shipped_policy());
    let mut plugin = host.load(asset_plugin()).unwrap();

    let results = plugin
        .call(
            "apply",
            &[
                image(2, 2, &SOURCE),
                Val::List(vec![resize_step(60_000, 60_000)]),
            ],
        )
        .unwrap();

    // The 32 MiB bound is the WIT's, not the plugin's: `operation.resize` in
    // `asset.wit` states it so four guests refuse the same extents.
    let (case, payload) = expect_err_text(&results);
    assert_eq!(case, "malformed");
    assert!(payload.contains("33554432-byte ceiling"), "{payload}");
}

#[test]
fn the_shipped_policy_grants_exactly_what_the_component_asks_for() {
    // The policy is derived from the component's declared imports, so it can
    // go stale when the guest is rebuilt against a different toolchain. This is
    // the check that says so, and it is also the assertion that `${plugin_dir}`
    // expands to something that exists.
    let manifest = shipped_policy();
    assert_eq!(manifest.permissions.fs.read, ["${plugin_dir}/luts"]);
    assert!(manifest.permissions.fs.write.is_empty());

    let mut expanded = manifest;
    let mut vars = BTreeMap::new();
    vars.insert(
        "plugin_dir".to_string(),
        asset_plugin()
            .parent()
            .unwrap()
            .canonicalize()
            .unwrap()
            .display()
            .to_string(),
    );
    expanded.substitute(&vars).unwrap();
    assert!(
        PathBuf::from(&expanded.permissions.fs.read[0]).is_dir(),
        "the grant should name a directory that exists: {}",
        expanded.permissions.fs.read[0]
    );
}
