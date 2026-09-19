//! The preview world: a file-format decoder as an untrusted plugin.
//!
//! What this suite is for, in order of importance:
//!
//! 1. **The bomb.** `examples/fixtures/preview/bomb.qoi` is a valid file whose
//!    header claims 16384 x 16384 -- a 1 GiB image -- with nothing else wrong.
//!    The decoder does not police that, on purpose: it cannot know the host's
//!    budget. `limits.memory` refuses the allocation and the host learns *that*,
//!    not "wasm trap: unreachable". This is the reason the example exists.
//! 2. **Correctness against a reference.** The valid fixture was encoded by the
//!    `qoi` crate, and this suite decodes it again with that crate and compares
//!    byte for byte. Two implementations that agree, one of which is not ours.
//! 3. **The failure cases a real viewer hits** -- a renamed file, a download
//!    cut off -- come back as typed failures the host can branch on.
//!
//! The Rust guest is built here, as `asset_e2e` builds its reference guest.
//! Other languages join the list as they are written and are skipped when
//! absent, held to the same assertions when present.

use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

use watoots::{ErrorKind, Host, Manifest, Val};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn fixture(name: &str) -> Vec<u8> {
    let path = repo_root().join("examples/fixtures/preview").join(name);
    std::fs::read(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
}

struct Guest {
    name: &'static str,
    wasm: PathBuf,
    policy: PathBuf,
    /// One host per guest for the whole process, so the compiled component is
    /// built once and every test's `open` is an instantiation. With a fresh
    /// host per test the 12 MB JavaScript and 18 MB Python guests were compiled
    /// eight times each and the suite took 221 seconds; this is the in-memory
    /// compiled-component cache doing what it exists for. A `Host` is
    /// `Send + Sync` and each `load` gets its own `Store`, so tests running in
    /// parallel share nothing but machine code.
    host: OnceLock<Host>,
}

impl Guest {
    fn host(&self) -> &Host {
        self.host.get_or_init(|| {
            Host::builder()
                .manifest(Manifest::from_file(&self.policy).expect("shipped policy parses"))
                .build()
                .expect("host")
        })
    }

    fn open(&self) -> watoots::Plugin {
        self.host()
            .load(&self.wasm)
            .unwrap_or_else(|err| panic!("{}: {}", self.name, err.message()))
    }

    fn decode(&self, plugin: &mut watoots::Plugin, bytes: &[u8]) -> watoots::Result<Vec<Val>> {
        let data = Val::List(bytes.iter().map(|&b| Val::U8(b)).collect());
        plugin.call("decode", &[data])
    }
}

fn build_rust_qoi() -> &'static PathBuf {
    static ARTIFACT: OnceLock<PathBuf> = OnceLock::new();
    ARTIFACT.get_or_init(|| {
        let crate_dir = repo_root().join("examples/plugins/rust-qoi");
        let status = Command::new(env!("CARGO"))
            .args(["build", "--manifest-path"])
            .arg(crate_dir.join("Cargo.toml"))
            .args(["--target", "wasm32-wasip2", "--release"])
            .status()
            .expect("running cargo to build the qoi plugin");
        assert!(status.success(), "failed to build rust-qoi");
        let built = crate_dir.join("target/wasm32-wasip2/release/rust_qoi.wasm");
        let installed = crate_dir.join("rust_qoi.wasm");
        std::fs::copy(&built, &installed).expect("installing rust_qoi.wasm");
        installed
    })
}

fn build_rust_farbfeld() -> &'static PathBuf {
    static ARTIFACT: OnceLock<PathBuf> = OnceLock::new();
    ARTIFACT.get_or_init(|| {
        let crate_dir = repo_root().join("examples/plugins/rust-farbfeld");
        let status = Command::new(env!("CARGO"))
            .args(["build", "--manifest-path"])
            .arg(crate_dir.join("Cargo.toml"))
            .args(["--target", "wasm32-wasip2", "--release"])
            .status()
            .expect("running cargo to build the farbfeld plugin");
        assert!(status.success(), "failed to build rust-farbfeld");
        let built = crate_dir.join("target/wasm32-wasip2/release/rust_farbfeld.wasm");
        let installed = crate_dir.join("rust_farbfeld.wasm");
        std::fs::copy(&built, &installed).expect("installing rust_farbfeld.wasm");
        installed
    })
}

/// The second format's decoder. Not in `guests()`: those all decode QOI and
/// are held to the same QOI assertions, and this one decodes something else.
fn farbfeld() -> &'static Guest {
    static GUEST: OnceLock<Guest> = OnceLock::new();
    GUEST.get_or_init(|| Guest {
        name: "rust-farbfeld",
        wasm: build_rust_farbfeld().clone(),
        policy: repo_root().join("examples/policies/rust-farbfeld.toml"),
        host: OnceLock::new(),
    })
}

fn guests() -> &'static [Guest] {
    static GUESTS: OnceLock<Vec<Guest>> = OnceLock::new();
    GUESTS.get_or_init(|| {
        let root = repo_root();
        let mut built = vec![Guest {
            name: "rust-qoi",
            wasm: build_rust_qoi().clone(),
            policy: root.join("examples/policies/rust-qoi.toml"),
            host: OnceLock::new(),
        }];
        // The other three need toolchains `rustup` does not install -- wasi-sdk,
        // an npm tree, a Python venv -- so each is present on a machine that
        // has run `tools/build-plugins.sh` for it and absent otherwise. Absent
        // is not a failure; present is held to every assertion below, including
        // byte-for-byte agreement with the reference decoder and the bomb.
        for (name, wasm, policy) in [
            (
                "cpp-qoi",
                "examples/plugins/cpp-qoi/cpp_qoi.wasm",
                "examples/policies/cpp-qoi.toml",
            ),
            (
                "js-qoi",
                "examples/plugins/js-qoi/js_qoi.wasm",
                "examples/policies/js-qoi.toml",
            ),
            (
                "py-qoi",
                "examples/plugins/py-qoi/py_qoi.wasm",
                "examples/policies/py-qoi.toml",
            ),
        ] {
            let wasm = root.join(wasm);
            if wasm.is_file() {
                built.push(Guest {
                    name,
                    wasm,
                    policy: root.join(policy),
                    host: OnceLock::new(),
                });
            }
        }
        built
    })
}

/// Pull `(width, height, pixels)` out of `ok(image)`.
fn expect_image(results: &[Val]) -> (u32, u32, Vec<u8>) {
    let Some(Val::Result(Ok(Some(image)))) = results.first() else {
        panic!("expected ok(image), got {results:?}");
    };
    let Val::Record(fields) = image.as_ref() else {
        panic!("expected a record, got {image:?}");
    };
    let field = |name: &str| {
        fields
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v)
            .unwrap_or_else(|| panic!("no field {name}"))
    };
    let Val::U32(w) = field("width") else {
        panic!("width")
    };
    let Val::U32(h) = field("height") else {
        panic!("height")
    };
    let Val::List(px) = field("pixels") else {
        panic!("pixels")
    };
    let bytes = px
        .iter()
        .map(|v| match v {
            Val::U8(b) => *b,
            other => panic!("pixel byte: {other:?}"),
        })
        .collect();
    (*w, *h, bytes)
}

/// Pull the case name out of `err(failure)`.
fn expect_failure(results: &[Val]) -> String {
    let Some(Val::Result(Err(Some(failure)))) = results.first() else {
        panic!("expected err(failure), got {results:?}");
    };
    let Val::Variant(case, _) = failure.as_ref() else {
        panic!("expected a variant, got {failure:?}");
    };
    case.clone()
}

#[test]
fn every_guest_decodes_the_reference_image_byte_for_byte() {
    // The oracle is the `qoi` crate, which also produced the fixture. Our
    // decoder and theirs have to agree on all 4096 bytes.
    let bytes = fixture("blocks.qoi");
    let (header, reference) =
        qoi::decode_to_vec(&bytes).expect("the reference decodes its own output");
    assert_eq!((header.width, header.height), (32, 32));
    assert_eq!(
        reference,
        fixture("blocks.rgba"),
        "the committed .rgba is the reference's output"
    );

    for guest in guests() {
        let mut plugin = guest.open();
        let (w, h, pixels) = expect_image(&guest.decode(&mut plugin, &bytes).unwrap());
        assert_eq!((w, h), (32, 32), "{}", guest.name);
        assert_eq!(pixels.len(), 32 * 32 * 4, "{}", guest.name);
        assert_eq!(
            pixels, reference,
            "{}: disagrees with the reference decoder",
            guest.name
        );
    }
}

#[test]
fn the_bomb_is_refused_by_the_manifest_and_reported_as_the_ceiling() {
    // A header claiming 1 GiB, otherwise a valid file. The decoder passes its
    // own overflow check -- this is not corrupt -- and asks for the memory.
    // The sandbox says no, and says *that*, not "unreachable".
    let bomb = fixture("bomb.qoi");
    for guest in guests() {
        let mut plugin = guest.open();
        let err = guest
            .decode(&mut plugin, &bomb)
            .expect_err("1 GiB under a 64 MiB policy");
        assert_eq!(
            err.kind(),
            ErrorKind::LimitExceeded,
            "{}: a ceiling, not a trap: {}",
            guest.name,
            err.message()
        );
        assert!(
            err.message().contains("limits.memory"),
            "{}: {}",
            guest.name,
            err.message()
        );
        // The host process did not grow to meet it: the sandbox refused before
        // any allocation, so this test's own memory is a fair proxy. If the
        // bomb ever went through, 1 GiB would have been committed here.
    }
}

#[test]
fn a_renamed_file_is_not_this_format_and_costs_nothing() {
    // Same bytes as the valid image with the magic replaced: what a `.qoi`
    // that is actually something else looks like. The host's dispatch signal.
    let other = fixture("not-qoi.bin");
    for guest in guests() {
        let mut plugin = guest.open();
        assert_eq!(
            expect_failure(&guest.decode(&mut plugin, &other).unwrap()),
            "not-this-format",
            "{}",
            guest.name
        );
        // And `sniff` says the same from the prefix alone.
        let prefix = Val::List(other[..8].iter().map(|&b| Val::U8(b)).collect());
        assert_eq!(
            plugin.call("sniff", &[prefix]).unwrap(),
            vec![Val::Bool(false)]
        );
        let real = Val::List(b"qoif\0\0\0\x20".iter().map(|&b| Val::U8(b)).collect());
        assert_eq!(
            plugin.call("sniff", &[real]).unwrap(),
            vec![Val::Bool(true)]
        );
    }
}

#[test]
fn a_download_cut_off_halfway_is_truncated_not_corrupt() {
    let cut = fixture("truncated.qoi");
    for guest in guests() {
        let mut plugin = guest.open();
        assert_eq!(
            expect_failure(&guest.decode(&mut plugin, &cut).unwrap()),
            "truncated",
            "{}",
            guest.name
        );
    }
}

#[test]
fn every_shipped_preview_policy_grants_exactly_what_its_decoder_imports() {
    // The decoder asks for nothing; what the policy grants is what the
    // toolchain dragged in. `inspect` has to be satisfied and nothing more.
    for guest in guests() {
        let wasm = std::fs::read(&guest.wasm).unwrap();
        let report = guest.host().inspect(&wasm).unwrap();
        assert!(
            report.is_satisfied(),
            "{}: {}",
            guest.name,
            report.describe()
        );
    }
}

// ---------------------------------------------------------------------------
// Two formats, so that dispatch is between two codecs and not two copies.

#[test]
fn the_farbfeld_decoder_reproduces_the_reference_exactly() {
    // blocks.ff was widened from blocks.rgba as `b << 8 | b`; taking the high
    // byte back must give the reference decoder's output to the byte.
    let guest = farbfeld();
    let mut plugin = guest.open();
    let (w, h, pixels) = expect_image(&guest.decode(&mut plugin, &fixture("blocks.ff")).unwrap());
    assert_eq!((w, h), (32, 32));
    assert_eq!(pixels, fixture("blocks.rgba"));
}

#[test]
fn each_decoder_declines_the_other_format_from_the_prefix() {
    // This is what "dispatch on magic bytes" means, and the previous version
    // of this suite never showed it: both installed decoders were QOI, so
    // the first always won. Now one says no and the other says yes, both from
    // the first sixteen bytes and without allocating.
    let ff = fixture("blocks.ff");
    let qoi = fixture("blocks.qoi");
    let prefix = |bytes: &[u8]| Val::List(bytes[..16].iter().map(|&b| Val::U8(b)).collect());

    let ff_guest = farbfeld();
    let mut ff_plugin = ff_guest.open();
    assert_eq!(
        ff_plugin.call("sniff", &[prefix(&ff)]).unwrap(),
        vec![Val::Bool(true)]
    );
    assert_eq!(
        ff_plugin.call("sniff", &[prefix(&qoi)]).unwrap(),
        vec![Val::Bool(false)]
    );
    // And `decode` agrees with `sniff` about what is not its business.
    assert_eq!(
        expect_failure(&ff_guest.decode(&mut ff_plugin, &qoi).unwrap()),
        "not-this-format"
    );

    for guest in guests() {
        let mut plugin = guest.open();
        assert_eq!(
            plugin.call("sniff", &[prefix(&qoi)]).unwrap(),
            vec![Val::Bool(true)],
            "{}",
            guest.name
        );
        assert_eq!(
            plugin.call("sniff", &[prefix(&ff)]).unwrap(),
            vec![Val::Bool(false)],
            "{}",
            guest.name
        );
        assert_eq!(
            expect_failure(&guest.decode(&mut plugin, &ff).unwrap()),
            "not-this-format",
            "{}",
            guest.name
        );
    }
}

#[test]
fn a_farbfeld_size_lie_is_truncated_before_any_allocation() {
    // The point farbfeld makes that QOI cannot: its size follows from its
    // header, so a header claiming 16384 x 16384 in an 8 KB file is provably
    // short before a byte of pixels is allocated. The *decoder* refuses this
    // one, with `truncated`; the QOI bomb is the sandbox's to refuse, because
    // a chunk stream gives the decoder no way to know up front. Both are fine
    // outcomes. Only one was in the plugin author's hands.
    let mut lie = fixture("blocks.ff");
    lie[8..12].copy_from_slice(&16384u32.to_be_bytes());
    lie[12..16].copy_from_slice(&16384u32.to_be_bytes());

    let guest = farbfeld();
    let mut plugin = guest.open();
    let results = guest
        .decode(&mut plugin, &lie)
        .expect("a typed failure, not a ceiling");
    assert_eq!(expect_failure(&results), "truncated");
    // Under the same 64 MiB policy the QOI bomb hits, and no ceiling was spent:
    // the decoder never asked.
    assert!(plugin.stats().peak_memory_bytes < 64 * 1024 * 1024);
}
