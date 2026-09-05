//! The asset plugins, end to end — and the capability they need.
//!
//! `plugin_e2e.rs` proves the typed boundary works with a plugin that needs
//! nothing. This one is the other half: an asset guest opens a file itself, so
//! it is the sample that shows a grant being required, scoped, and — the part
//! that matters — enforced at *load*, before any guest code runs.
//!
//! It also pins the arithmetic, for **every guest that has been built**.
//! `examples/wit/asset/asset.wit` names this file as part of its own
//! definition, because several of the world's obligations — `resize`'s 32 MiB
//! bound, `gain`'s clamp, the rounding in every operation — are doc comments
//! that WIT cannot express. A guest that ignores one still compiles. It does
//! not get past here.
//!
//! Every expected value below is computed by hand from the rule documented in
//! `examples/plugins/rust-asset/src/lib.rs`, not captured from a run, so a
//! guest that disagrees fails against the contract rather than being blessed by
//! whatever the first implementation happened to produce.
//!
//! # Which guests run
//!
//! The Rust guest is built here, from `rustup`'s `wasm32-wasip2` target, so it
//! is always present. The others need toolchains that do not come from
//! `rustup` — C++ needs wasi-sdk and `wit-bindgen` — so they are picked up when
//! `tools/build-plugins.sh` has produced them and skipped when it has not. A
//! guest nobody has built must not fail the suite; a guest that *is* built gets
//! no discount.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};

use watoots::{ErrorKind, Host, Manifest, Plugin, Val};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

// ---------------------------------------------------------------------------
// The guests
// ---------------------------------------------------------------------------

/// One built implementation of `asset-plugin`.
///
/// The three paths are the whole of what a conformance case needs: the
/// component, the policy that ships beside it, and the `luts/` directory that
/// policy grants. Everything else about a guest — which language, which
/// toolchain, which set of WASI imports its runtime dragged in — is exactly
/// what the world is supposed to make irrelevant.
struct Guest {
    /// The name the guest's `describe` returns, which is also what a failed
    /// assertion is labelled with. The one field where guests legitimately
    /// differ, so it is asserted rather than compared.
    name: &'static str,
    wasm: PathBuf,
    policy: PathBuf,
    luts: PathBuf,
}

impl Guest {
    /// The policy that ships with the example, read from disk rather than
    /// inlined.
    ///
    /// If a shipped policy stops granting what its guest needs, this suite is
    /// where that shows up — which is the only way a shipped policy stays true.
    /// It is also why the two policies are read separately instead of one being
    /// reused: `cpp-asset.toml` grants `clocks = "wall"` and `rust-asset.toml`
    /// does not, because wasi-libc links a clock Rust's `std` does not.
    fn manifest(&self) -> Manifest {
        Manifest::from_file(&self.policy)
            .unwrap_or_else(|err| panic!("reading {}: {err}", self.policy.display()))
    }

    /// The same policy with the filesystem grant taken away, and nothing else.
    fn without_filesystem(&self) -> Manifest {
        let mut manifest = self.manifest();
        manifest.permissions.fs.read.clear();
        manifest.permissions.fs.write.clear();
        manifest
    }

    /// A loaded plugin under the shipped policy, logging into `logged`.
    fn open(&self, logged: Arc<Mutex<Vec<String>>>) -> Plugin {
        host_with(self.manifest(), logged)
            .load(&self.wasm)
            .unwrap_or_else(|err| panic!("loading {}: {}", self.name, err.message()))
    }

    /// A loaded plugin whose log lines go nowhere.
    fn quiet(&self) -> Plugin {
        self.open(Arc::new(Mutex::new(Vec::new())))
    }

    /// The lookup table shipped beside the guest, spelled the way its grant
    /// spells it.
    ///
    /// Each guest can only read its own `luts/`, because the grant is a
    /// directory under `${plugin_dir}`. The tables are byte-identical on
    /// purpose — [`every_guest_ships_the_same_lookup_table`] is what says so.
    fn lut(&self) -> String {
        self.luts.join("sepia.lut").display().to_string()
    }

    /// A path inside the granted directory, removed when the test ends.
    fn scratch(&self, name: &str) -> Scratch {
        Scratch(self.luts.join(name))
    }
}

/// Every asset guest that has been built.
///
/// Ordered with the Rust guest first, because it is the reference
/// implementation and [`every_guest_agrees_byte_for_byte`] compares the rest
/// against it.
fn guests() -> &'static [Guest] {
    static GUESTS: OnceLock<Vec<Guest>> = OnceLock::new();
    GUESTS.get_or_init(|| {
        let root = repo_root();
        let mut built = vec![Guest {
            name: "rust-asset",
            wasm: build_rust_asset().clone(),
            policy: root.join("examples/policies/rust-asset.toml"),
            luts: root.join("examples/plugins/rust-asset/luts"),
        }];

        // wasi-sdk is a 172MB tarball with no Homebrew formula and `wit-bindgen`
        // is a separate `cargo install`, so this guest is present on a machine
        // that has run `tools/build-plugins.sh cpp-asset` and absent otherwise.
        // Absent is not a failure — `cargo test` has to pass on a clean
        // checkout with only `rustup` — but present is not a discount either.
        let cpp = root.join("examples/plugins/cpp-asset/cpp_asset.wasm");
        if cpp.is_file() {
            built.push(Guest {
                name: "cpp-asset",
                wasm: cpp,
                policy: root.join("examples/policies/cpp-asset.toml"),
                luts: root.join("examples/plugins/cpp-asset/luts"),
            });
        }

        built
    })
}

/// Build the Rust asset plugin once per test binary and return the artifact.
///
/// The artifact is copied next to its `luts/` directory, exactly as
/// `tools/build-plugins.sh` does, because `${plugin_dir}` in the shipped policy
/// expands to the directory a component was *loaded from*. Loading straight out
/// of `target/` would make the policy under test a different policy.
fn build_rust_asset() -> &'static PathBuf {
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

/// The Rust guest on its own, for the cases that are about *it* rather than
/// about the world — the shipped-policy shape, mostly.
fn rust_asset() -> &'static Guest {
    &guests()[0]
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

/// An image as a test sees it: extent and pixels.
///
/// Named because the cross-guest comparisons hold one of these as the
/// reference answer, and `Option<(&str, (u32, u32, Vec<u8>)))` is a type
/// clippy is right to object to.
type Rendered = (u32, u32, Vec<u8>);

/// The `ok` arm of `result<image, failure>`, as `(width, height, pixels)`.
fn expect_ok(results: &[Val]) -> Rendered {
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

// ---------------------------------------------------------------------------
// Conformance: every built guest, the same assertions.
// ---------------------------------------------------------------------------

#[test]
fn describe_names_the_plugin_and_every_step_it_implements() {
    for guest in guests() {
        let mut plugin = guest.quiet();

        let results = plugin.call("describe", &[]).unwrap();
        let Some(Val::Record(fields)) = results.first() else {
            panic!(
                "{}: expected a plugin-info record, got {results:?}",
                guest.name
            );
        };
        let field = |name: &str| {
            fields
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
                .unwrap_or_else(|| panic!("no field {name} in {fields:?}"))
        };

        // The one thing guests are allowed to disagree about, so it is checked
        // against the guest rather than across guests.
        assert_eq!(field("name"), Val::String(guest.name.into()));

        // `list<operation-kind>`, so these are bare enum cases and there is
        // nothing else in them. Asserting the whole list is the assertion —
        // when this was `list<operation>` the same check had to reach past
        // invented payloads that a host was expected to know to ignore. The
        // *order* is asserted too: nothing in the WIT requires it, but four
        // guests that all list the steps in the world's order are four guests
        // whose `describe` a host can compare without sorting.
        let Val::List(kinds) = field("supports") else {
            panic!("{}: expected a list of operation kinds", guest.name);
        };
        assert_eq!(
            kinds,
            ["grayscale", "invert", "gain", "resize", "lut"]
                .map(|case| Val::Enum(case.to_string()))
                .to_vec(),
            "{}",
            guest.name
        );
    }
}

#[test]
fn grayscale_is_rec_601_luma_in_fixed_point() {
    // The single most copied rule in this world, so it gets its own assertion
    // with the arithmetic spelled out beside `GRAY`. A guest that reaches for
    // `0.299f * r + ...` instead disagrees here, on the mid-tone pixel.
    for guest in guests() {
        let mut plugin = guest.quiet();

        let results = plugin
            .call(
                "apply",
                &[image(2, 2, &SOURCE), Val::List(vec![step("grayscale")])],
            )
            .unwrap();
        assert_eq!(expect_ok(&results), (2, 2, GRAY.to_vec()), "{}", guest.name);
    }
}

#[test]
fn a_pipeline_applies_its_steps_in_order() {
    for guest in guests() {
        let mut plugin = guest.quiet();

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
        assert_eq!(
            expect_ok(&results),
            (2, 2, expected.to_vec()),
            "{}",
            guest.name
        );
    }
}

#[test]
fn order_matters_and_the_test_can_tell() {
    // The same two steps the other way round. If `apply` ever ran steps in
    // some other order — or folded them — this is what would catch it.
    for guest in guests() {
        let mut plugin = guest.quiet();

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
        assert_eq!(
            expect_ok(&inverted_then_gray),
            (2, 2, expected.to_vec()),
            "{}",
            guest.name
        );
    }
}

#[test]
fn gain_clamps_its_factor_and_treats_nan_as_zero() {
    // `gain.factor` in `asset.wit` puts the clamp and the NaN rule on the
    // *guest*, and says why: nothing in the type system makes a host do it, NaN
    // is representable, and the wrong answer differs by language. Rust's
    // `as u8` saturates; C++'s float-to-integer conversion out of range is
    // undefined behaviour, so a C++ guest that skipped the clamp could produce
    // anything at all — including the right answer on the day it was tested.
    for guest in guests() {
        let mut plugin = guest.quiet();

        // Over the top of the range: 9.0 clamps to 4.0, so 200 -> 800 -> 255
        // (capped), 100 -> 400 -> 255, 50 -> 200, and 10 -> 40.
        let clamped_high = plugin
            .call(
                "apply",
                &[
                    image(2, 2, &SOURCE),
                    Val::List(vec![
                        gain_step("red", 9.0),
                        gain_step("green", 9.0),
                        gain_step("blue", 9.0),
                    ]),
                ],
            )
            .unwrap();
        #[rustfmt::skip]
        let expected = [
             40,  80, 120,
            255, 255, 200,
            255, 255, 255,
              0,   0,   0,
        ];
        assert_eq!(
            expect_ok(&clamped_high),
            (2, 2, expected.to_vec()),
            "{} clamping a factor of 9.0 to 4.0",
            guest.name
        );

        // Below it, and NaN. Both become 0.0, and `floor(c * 0.0 + 0.5)` is 0
        // for every sample — so the red channel goes flat and the other two are
        // untouched, which is also the assertion that `gain` stays in its lane.
        for factor in [-3.0, f32::NAN] {
            let zeroed = plugin
                .call(
                    "apply",
                    &[
                        image(2, 2, &SOURCE),
                        Val::List(vec![gain_step("red", factor)]),
                    ],
                )
                .unwrap();
            #[rustfmt::skip]
            let expected = [
                0,  20,  30,
                0, 100,  50,
                0, 255, 255,
                0,   0,   0,
            ];
            assert_eq!(
                expect_ok(&zeroed),
                (2, 2, expected.to_vec()),
                "{} with a factor of {factor}",
                guest.name
            );
        }
    }
}

#[test]
fn resize_is_nearest_neighbour_and_biased_to_the_top_left() {
    for guest in guests() {
        let mut plugin = guest.quiet();

        let results = plugin
            .call(
                "apply",
                &[image(2, 2, &SOURCE), Val::List(vec![resize_step(3, 3)])],
            )
            .unwrap();

        // sx = dx * 2 / 3 -> 0, 0, 1;  sy likewise. A centre-sampling filter
        // would give 0, 1, 1 instead, so this asserts the choice and not just
        // the size.
        #[rustfmt::skip]
        let expected = [
            10, 20, 30,   10, 20, 30,   200, 100, 50,
            10, 20, 30,   10, 20, 30,   200, 100, 50,
            255, 255, 255, 255, 255, 255, 0, 0, 0,
        ];
        assert_eq!(
            expect_ok(&results),
            (3, 3, expected.to_vec()),
            "{}",
            guest.name
        );
    }
}

#[test]
fn a_degenerate_resize_is_an_empty_image_and_not_an_error() {
    // Also from the WIT: "a zero-width or zero-height destination is a
    // zero-pixel image, not an error", and sampling a zero-area source into a
    // destination with area is `malformed`. Two edges a guest can only get
    // right by having read the world, and the second one is a division by zero
    // in every language if it is not checked.
    for guest in guests() {
        let mut plugin = guest.quiet();

        let empty = plugin
            .call(
                "apply",
                &[image(2, 2, &SOURCE), Val::List(vec![resize_step(4, 0)])],
            )
            .unwrap();
        assert_eq!(expect_ok(&empty), (4, 0, Vec::new()), "{}", guest.name);

        let from_nothing = plugin
            .call(
                "apply",
                &[image(0, 5, &[]), Val::List(vec![resize_step(2, 2)])],
            )
            .unwrap();
        let (case, payload) = expect_err_text(&from_nothing);
        assert_eq!(case, "malformed", "{}", guest.name);
        assert_eq!(
            payload, "cannot resize 0x5 to 2x2: no source pixels to sample",
            "{}",
            guest.name
        );
    }
}

#[test]
fn a_lut_step_reads_its_table_through_the_granted_directory() {
    for guest in guests() {
        let logged = Arc::new(Mutex::new(Vec::new()));
        let mut plugin = guest.open(Arc::clone(&logged));

        let results = plugin
            .call(
                "apply",
                &[
                    image(2, 2, &SOURCE),
                    Val::List(vec![step("grayscale"), lut_step(&guest.lut())]),
                ],
            )
            .unwrap();

        // GRAY is 18, 124, 255, 0 in every channel, and `luts/sepia.lut` maps
        // those to (24,22,17), (168,149,116), (255,255,239) and (0,0,0). The
        // table is applied per channel: the red output comes from the red
        // column of the entry the red input selects, and never from green or
        // blue.
        #[rustfmt::skip]
        let expected = [
             24,  22,  17,
            168, 149, 116,
            255, 255, 239,
              0,   0,   0,
        ];
        assert_eq!(
            expect_ok(&results),
            (2, 2, expected.to_vec()),
            "{}",
            guest.name
        );

        // Nothing was logged at error: the read succeeded. The line itself is
        // conformance too — it is an import crossing, and the recorder compares
        // those across guests as well as return values.
        let logged = logged.lock().unwrap().clone();
        assert_eq!(
            logged,
            ["info: apply: 2x2, 2 step(s)"],
            "{}: {logged:?}",
            guest.name
        );
    }
}

#[test]
fn without_the_filesystem_grant_the_plugin_does_not_load_at_all() {
    // The point of the example. `lut` never runs, no pixel is touched, and the
    // failure is not a runtime denial deep inside a pipeline: the component
    // *declares* `wasi:filesystem` in its binary, so the mismatch is visible
    // before instantiation — in whatever language the binary was produced from.
    for guest in guests() {
        let host = host_with(guest.without_filesystem(), Arc::new(Mutex::new(Vec::new())));

        let err = host.load(&guest.wasm).unwrap_err();
        assert_eq!(
            err.kind(),
            ErrorKind::PermissionDenied,
            "{}: {}",
            guest.name,
            err.message()
        );
        assert!(
            err.message().contains("wasi:filesystem"),
            "{}: the denial should name the interface: {}",
            guest.name,
            err.message()
        );

        // And `inspect` says the same thing without loading anything, which is
        // the install-time version of this check.
        let wasm = std::fs::read(&guest.wasm).unwrap();
        let report = host.inspect(&wasm).unwrap();
        assert!(
            !report.is_satisfied(),
            "{}: {}",
            guest.name,
            report.describe()
        );
    }
}

#[test]
fn a_lut_outside_the_granted_directory_is_unreadable_rather_than_fatal() {
    // The grant is scoped to `luts/`, so a table elsewhere is refused by WASI
    // — and the plugin turns that into an answer rather than a trap.
    for guest in guests() {
        let logged = Arc::new(Mutex::new(Vec::new()));
        let mut plugin = guest.open(Arc::clone(&logged));

        let outside = guest.policy.display().to_string();
        let results = plugin
            .call(
                "apply",
                &[image(2, 2, &SOURCE), Val::List(vec![lut_step(&outside)])],
            )
            .unwrap();

        let (path, reason) = expect_unreadable(&results);
        assert_eq!(
            path, outside,
            "{}: the path is the one that was asked for",
            guest.name
        );

        // The reason travels in the return value, so a caller with no log sink
        // still learns why. It also says the thing that is actually true here:
        // WASI reports a path outside every preopen as *not found*, so the
        // plugin cannot distinguish "denied" from "missing" and does not
        // pretend to.
        //
        // Asserted by substring rather than equality, and this is the one place
        // in the suite where that is a statement about the world rather than
        // laziness: the middle of the reason is the platform's rendering of an
        // errno, and a guest gets that from whatever runtime it was built on.
        // Rust's `io::Error` and C's `strerror` happen to agree here — both
        // guests are linked against the same wasi-libc — but nothing in the WIT
        // makes them, and a guest whose runtime words `ENOENT` differently is
        // still conforming.
        assert!(
            reason.contains("cannot open it"),
            "{}: {reason}",
            guest.name
        );
        assert!(
            reason.contains("check the manifest"),
            "{}: {reason}",
            guest.name
        );

        // The log line is a courtesy rather than the only channel, and there is
        // exactly one of it.
        let logged = logged.lock().unwrap().clone();
        assert_eq!(
            logged.len(),
            2,
            "{}: one info, one error: {logged:?}",
            guest.name
        );
        assert!(
            logged[1].starts_with("error: unreadable lookup table"),
            "{}: {logged:?}",
            guest.name
        );
    }
}

#[test]
fn a_corrupt_table_is_reported_rather_than_silently_clipped() {
    for guest in guests() {
        // A file inside the grant that is not a lookup table. Reading it
        // succeeds and parsing it must not.
        let scratch = guest.scratch("scratch-not-a-table.lut");
        std::fs::write(&scratch.0, "0 0 0\n1 1 1\n").unwrap();

        let logged = Arc::new(Mutex::new(Vec::new()));
        let mut plugin = guest.open(Arc::clone(&logged));

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
        // manifest already covers. Unlike the open failure above, this reason
        // is the guest's own prose all the way through, so it is compared
        // exactly and every guest owes the same sentence.
        let (path, reason) = expect_unreadable(&results);
        assert_eq!(path, scratch.0.display().to_string(), "{}", guest.name);
        assert_eq!(reason, "expected 256 entries, found 2", "{}", guest.name);

        // And the other end: a table with an entry too many is refused at the
        // 257th rather than read to the end. A grant is a directory, so "what
        // is in luts/" is not a closed set, and neither direction may be
        // tolerated quietly — a short table would clip highlights and a long
        // one would say nothing about which 256 entries won.
        let long = guest.scratch("scratch-too-long.lut");
        let mut text = String::new();
        for _ in 0..257 {
            text.push_str("0 0 0\n");
        }
        std::fs::write(&long.0, text).unwrap();

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
        assert_eq!(
            reason, "more than 256 entries: a 257th appears on line 257",
            "{}",
            guest.name
        );

        // A line that is not three integers names the line and quotes it. The
        // quoting is Rust's `{:?}`, which is a guest-visible choice — a C guest
        // has to reproduce the escaping rather than inherit it — so a plain
        // ASCII case pins it. Line 3 of the file, the third entry.
        let bad = guest.scratch("scratch-bad-line.lut");
        std::fs::write(&bad.0, "0 0 0\n1 1 1\n2 2 300\n").unwrap();

        let results = plugin
            .call(
                "apply",
                &[
                    image(2, 2, &SOURCE),
                    Val::List(vec![lut_step(&bad.0.display().to_string())]),
                ],
            )
            .unwrap();
        let (_, reason) = expect_unreadable(&results);
        assert_eq!(
            reason, "line 3 is not three integers in 0..=255: \"2 2 300\"",
            "{}",
            guest.name
        );
    }
}

#[test]
fn a_malformed_image_is_a_failure_and_not_a_trap() {
    for guest in guests() {
        let mut plugin = guest.quiet();

        // 2x2 needs 12 bytes; this is 11. The distinction that matters is that
        // `call` returns `Ok` — the guest answered — and the answer is `err`.
        let results = plugin
            .call(
                "apply",
                &[image(2, 2, &SOURCE[..11]), Val::List(vec![step("invert")])],
            )
            .unwrap();

        let (case, payload) = expect_err_text(&results);
        assert_eq!(case, "malformed", "{}", guest.name);
        assert_eq!(payload, "2x2 is 12 bytes of RGB8, got 11", "{}", guest.name);

        // The plugin is still usable afterwards, which a trap would not leave
        // true. An empty pipeline also has to hand the pixels back unchanged —
        // which in a C guest means the input buffer it took ownership of comes
        // straight back out, and in a leaking one would be a copy.
        let ok = plugin
            .call("apply", &[image(2, 2, &SOURCE), Val::List(vec![])])
            .unwrap();
        assert_eq!(expect_ok(&ok), (2, 2, SOURCE.to_vec()), "{}", guest.name);
    }
}

#[test]
fn an_absurd_resize_is_refused_instead_of_exhausting_memory() {
    for guest in guests() {
        let mut plugin = guest.quiet();

        let results = plugin
            .call(
                "apply",
                &[
                    image(2, 2, &SOURCE),
                    Val::List(vec![resize_step(60_000, 60_000)]),
                ],
            )
            .unwrap();

        // The 32 MiB bound is the WIT's, not the plugin's: `operation.resize`
        // in `asset.wit` states it so four guests refuse the same extents.
        let (case, payload) = expect_err_text(&results);
        assert_eq!(case, "malformed", "{}", guest.name);
        assert_eq!(
            payload,
            "resize to 60000x60000 would need 10800000000 bytes, \
             over the 33554432-byte ceiling",
            "{}",
            guest.name
        );

        // And a destination just barely over it, to pin the units. 3345 x 3344
        // is 11_185_680 *pixels*, comfortably under 33_554_432, and
        // 33_557_040 *bytes*, over it by 2608. A guest that forgot the `* 3`
        // accepts this one and allocates 32 MiB it was told not to; the extents
        // above are so absurd that they would be refused either way.
        let just_over = plugin
            .call(
                "apply",
                &[
                    image(2, 2, &SOURCE),
                    Val::List(vec![resize_step(3345, 3344)]),
                ],
            )
            .unwrap();
        let (case, payload) = expect_err_text(&just_over);
        assert_eq!(case, "malformed", "{}", guest.name);
        assert_eq!(
            payload,
            "resize to 3345x3344 would need 33557040 bytes, \
             over the 33554432-byte ceiling",
            "{}",
            guest.name
        );
    }
}

// ---------------------------------------------------------------------------
// Cross-guest: the same bytes, from different toolchains.
// ---------------------------------------------------------------------------

/// The pipeline the cross-guest comparison runs.
///
/// Every operation, in an order that makes each one's output the next one's
/// input, so a disagreement anywhere reaches the end. `resize` first, so the
/// later steps run over pixels a *guest* produced rather than over the ones the
/// host handed in; `gain` with a factor whose product is not exact in binary,
/// because that is where a `round` that is not `floor(x + 0.5)` shows up.
fn conformance_pipeline(lut: &str) -> Vec<Val> {
    vec![
        resize_step(5, 3),
        step("grayscale"),
        gain_step("green", 1.3),
        gain_step("blue", 0.7),
        step("invert"),
        lut_step(lut),
        gain_step("red", 4.0),
    ]
}

/// A source image big enough that nearest-neighbour sampling has choices to
/// make: 4x3, every byte distinct, so a transposed index or an off-by-one row
/// cannot survive.
fn conformance_source() -> Vec<u8> {
    (0..4u32 * 3 * 3)
        .map(|index| (index * 7 % 256) as u8)
        .collect()
}

#[test]
fn every_guest_agrees_byte_for_byte() {
    // The claim the world is built around, and the only test that can actually
    // check it: one pipeline, every built guest, compared to the Rust
    // reference. Skips silently when only one guest is built — a machine with
    // no wasi-sdk should not fail, but it also learns nothing, which is why
    // CONTRIBUTING.md says what to install.
    let source = conformance_source();
    let mut reference: Option<(&str, Rendered)> = None;

    for guest in guests() {
        let mut plugin = guest.quiet();
        let results = plugin
            .call(
                "apply",
                &[
                    image(4, 3, &source),
                    Val::List(conformance_pipeline(&guest.lut())),
                ],
            )
            .unwrap();
        let produced = expect_ok(&results);
        assert_eq!(
            (produced.0, produced.1),
            (5, 3),
            "{} resized to the wrong extent",
            guest.name
        );

        match &reference {
            None => reference = Some((guest.name, produced)),
            Some((first, expected)) => assert_eq!(
                &produced, expected,
                "{} and {} disagree on the same pipeline",
                first, guest.name
            ),
        }
    }
}

#[test]
fn every_guest_agrees_on_which_failure_not_on_its_wording() {
    // Guests must agree on *which* failure, not on how they word it — see
    // `failure` in the WIT, which says the prose is for people and the case is
    // for hosts.
    //
    // This test used to compare the messages exactly, and it passed. Getting it
    // to pass meant the C++ guest reproducing five behaviours of Rust's
    // standard library — its debug escaping, its integer parser's error text,
    // its line splitting, its UTF-8 error wording, its io::Error rendering —
    // none of which is about images. That is the cost of an accidental
    // contract, paid in the second guest and payable again in every one after,
    // so the contract was withdrawn rather than the bill paid twice more.
    let mut reference: Option<(&str, Vec<(String, String)>)> = None;

    for guest in guests() {
        let mut plugin = guest.quiet();
        let cases = vec![
            // A pixel buffer that disagrees with the dimensions.
            (image(3, 3, &SOURCE), Val::List(vec![step("invert")])),
            // A destination over the WIT's ceiling.
            (
                image(2, 2, &SOURCE),
                Val::List(vec![resize_step(4096, 4096)]),
            ),
            // A source with no pixels to sample.
            (image(7, 0, &[]), Val::List(vec![resize_step(1, 1)])),
        ];

        let produced = cases
            .into_iter()
            .map(|(input, steps)| {
                let results = plugin.call("apply", &[input, steps]).unwrap();
                expect_err_text(&results)
            })
            .collect::<Vec<_>>();

        for (case, (_, reason)) in produced.iter().enumerate() {
            assert!(
                !reason.trim().is_empty(),
                "{}: case {case} returned a failure with no reason; the prose is \
                 free but its absence is not — a host with no log sink would \
                 learn nothing",
                guest.name
            );
        }

        let cases: Vec<&String> = produced.iter().map(|(case, _)| case).collect();
        match &reference {
            None => reference = Some((guest.name, produced)),
            Some((first, expected)) => {
                let want: Vec<&String> = expected.iter().map(|(case, _)| case).collect();
                assert_eq!(
                    cases, want,
                    "{} and {} disagree on which failure a case produces",
                    first, guest.name
                );
            }
        }
    }
}

#[test]
fn every_guest_ships_the_same_lookup_table() {
    // A grant is a directory, so each guest can only read its own `luts/` — and
    // a `lut` step therefore compares two tables as well as two plugins. This
    // is what makes the comparison in `every_guest_agrees_byte_for_byte`
    // meaningful: if the tables drifted apart, that test would fail and blame
    // the arithmetic.
    //
    // Comments are ignored, because each table's header names its own plugin.
    let entries = |guest: &Guest| -> Vec<String> {
        let text = std::fs::read_to_string(guest.luts.join("sepia.lut"))
            .unwrap_or_else(|err| panic!("reading {}'s table: {err}", guest.name));
        text.lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(str::to_string)
            .collect()
    };

    let reference = entries(rust_asset());
    assert_eq!(reference.len(), 256, "the reference table is 256 entries");
    for guest in &guests()[1..] {
        assert_eq!(
            entries(guest),
            reference,
            "{}'s sepia.lut differs from {}'s",
            guest.name,
            rust_asset().name
        );
    }
}

// ---------------------------------------------------------------------------
// The shipped policies.
// ---------------------------------------------------------------------------

#[test]
fn every_shipped_policy_grants_exactly_what_its_component_asks_for() {
    // A policy is derived from its component's declared imports, so it can go
    // stale when that guest is rebuilt against a different toolchain. This is
    // the check that says so, and it is also the assertion that `${plugin_dir}`
    // expands to something that exists.
    //
    // The grant is asserted per guest and not compared across them, which is
    // the point of having two: `cpp-asset.toml` needs `clocks = "wall"` where
    // `rust-asset.toml` needs only `monotonic`, because wasi-libc links a clock
    // Rust's `std` does not. Identical behaviour, different bill.
    for guest in guests() {
        let manifest = guest.manifest();
        assert_eq!(
            manifest.permissions.fs.read,
            ["${plugin_dir}/luts"],
            "{}",
            guest.name
        );
        assert!(manifest.permissions.fs.write.is_empty(), "{}", guest.name);

        let mut expanded = manifest;
        let mut vars = BTreeMap::new();
        vars.insert(
            "plugin_dir".to_string(),
            guest
                .wasm
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
            "{}: the grant should name a directory that exists: {}",
            guest.name,
            expanded.permissions.fs.read[0]
        );

        // And the policy actually satisfies the binary. `watoots:asset/log` is
        // the one import a manifest cannot answer — the application serves it
        // with `host_func` — so this goes through a host that does.
        let host = host_with(guest.manifest(), Arc::new(Mutex::new(Vec::new())));
        let wasm = std::fs::read(&guest.wasm).unwrap();
        let report = host.inspect(&wasm).unwrap();
        assert!(
            report.is_satisfied(),
            "{}: {}",
            guest.name,
            report.describe()
        );
    }
}
