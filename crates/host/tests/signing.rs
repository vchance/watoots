//! Signature verification at load, end to end (ADR-0014).
//!
//! The fixtures are openssl's, not ours — see `fixtures/signing/README.md`.

use std::path::PathBuf;

use watoots::{ErrorKind, Host, Manifest, Val};

const COMPONENT: &str = include_str!("fixtures/signing/component.wat");
const SIGNATURE: &str = include_str!("fixtures/signing/component.wat.sig");
const SIGNER: &str = include_str!("fixtures/signing/signer.pub.pem");
const STRANGER: &str = include_str!("fixtures/signing/stranger.pub.pem");

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/signing")
}

/// A manifest trusting the given PEM keys, written as a real manifest would be.
fn host_trusting(pems: &[&str]) -> Host {
    let keys = pems
        .iter()
        .map(|pem| format!("\"\"\"\n{}\"\"\"", pem.trim_start()))
        .collect::<Vec<_>>()
        .join(", ");
    let toml = format!("[limits]\nfuel = 100_000_000\n\n[signature]\nkeys = [{keys}]\n");
    Host::builder()
        .manifest(Manifest::parse(&toml).unwrap_or_else(|e| panic!("{}\n{toml}", e.message())))
        .build()
        .unwrap()
}

fn unverified_host() -> Host {
    Host::builder()
        .manifest(Manifest::parse("[limits]\nfuel = 100_000_000\n").unwrap())
        .build()
        .unwrap()
}

fn answer(plugin: &mut watoots::Plugin) -> i32 {
    match plugin.call("answer", &[]).unwrap().as_slice() {
        [Val::S32(n)] => *n,
        other => panic!("expected one s32, got {other:?}"),
    }
}

#[test]
fn a_component_signed_by_a_trusted_key_loads_and_runs() {
    let host = host_trusting(&[SIGNER]);
    let mut plugin = host
        .load_binary_signed("signed", COMPONENT.as_bytes(), SIGNATURE.as_bytes())
        .expect("openssl signed exactly these bytes with exactly this key");
    assert_eq!(answer(&mut plugin), 42);
}

#[test]
fn no_signature_is_a_refusal_when_the_manifest_asks_for_one() {
    let host = host_trusting(&[SIGNER]);
    let err = host
        .load_binary("unsigned", COMPONENT.as_bytes())
        .expect_err("the manifest lists keys, so unsigned must not load");
    assert_eq!(err.kind(), ErrorKind::SignatureInvalid);
    // The message has to say what to do, because "no signature was supplied" is
    // otherwise indistinguishable from "the API is broken".
    assert!(err.message().contains(".sig"), "{}", err.message());
}

#[test]
fn a_valid_signature_by_an_untrusted_key_is_a_refusal() {
    // The signature verifies — against a key this manifest does not list. This
    // is the case a checksum cannot catch, and the reason to sign at all.
    let host = host_trusting(&[STRANGER]);
    let err = host
        .load_binary_signed("wrong-signer", COMPONENT.as_bytes(), SIGNATURE.as_bytes())
        .expect_err("signed, but not by anyone this manifest trusts");
    assert_eq!(err.kind(), ErrorKind::SignatureInvalid);
}

#[test]
fn tampering_with_one_byte_is_a_refusal() {
    let host = host_trusting(&[SIGNER]);
    let tampered = COMPONENT.replace("i32.const 42", "i32.const 43");
    assert_ne!(tampered, COMPONENT);
    let err = host
        .load_binary_signed("tampered", tampered.as_bytes(), SIGNATURE.as_bytes())
        .expect_err("the bytes changed after signing");
    assert_eq!(err.kind(), ErrorKind::SignatureInvalid);
}

#[test]
fn several_trusted_keys_are_what_a_rotation_looks_like() {
    // Old key and new key both listed, for as long as it takes to re-sign.
    let host = host_trusting(&[STRANGER, SIGNER]);
    let mut plugin = host
        .load_binary_signed("rotating", COMPONENT.as_bytes(), SIGNATURE.as_bytes())
        .expect("any listed key may be the signer");
    assert_eq!(answer(&mut plugin), 42);
}

#[test]
fn a_manifest_with_no_keys_checks_nothing_and_that_is_deliberate() {
    // The one place watoots does not default to deny. Every embedder that
    // upgrades without writing a `[signature]` section keeps working, which is
    // the whole reason for the asymmetry (ADR-0014).
    let host = unverified_host();
    let mut plugin = host.load_binary("unsigned", COMPONENT.as_bytes()).unwrap();
    assert_eq!(answer(&mut plugin), 42);
}

#[test]
fn a_signature_is_ignored_rather_than_rejected_when_nothing_is_being_verified() {
    // So an application can pass one unconditionally and let the manifest
    // decide whether it matters.
    let host = unverified_host();
    let mut plugin = host
        .load_binary_signed("whatever", COMPONENT.as_bytes(), b"not even base64 !!")
        .expect("no keys configured, so there is nothing to check it against");
    assert_eq!(answer(&mut plugin), 42);
}

#[test]
fn load_from_disk_finds_the_signature_beside_the_component() {
    let host = host_trusting(&[SIGNER]);
    let mut plugin = host
        .load(fixtures().join("component.wat"))
        .expect("component.wat.sig sits next to it, as cosign would write it");
    assert_eq!(answer(&mut plugin), 42);
}

#[test]
fn load_from_disk_refuses_when_the_signature_file_is_missing() {
    let dir = tempfile::tempdir().unwrap();
    let lonely = dir.path().join("component.wat");
    std::fs::write(&lonely, COMPONENT).unwrap();

    let host = host_trusting(&[SIGNER]);
    let err = host.load(&lonely).expect_err("no .sig beside it");
    assert_eq!(err.kind(), ErrorKind::SignatureInvalid);
}

#[test]
fn reload_re_verifies_and_that_is_the_point() {
    // ADR-0010 made reload re-run the capability check, because a plugin must
    // not gain a capability by being updated. The same argument applies to who
    // signed it, and it applies harder: the moment the code changes is the
    // moment publisher identity matters most. A check the application does
    // before calling `load` is a check reload skips.
    let host = host_trusting(&[SIGNER]);
    let mut plugin = host
        .load_binary_signed("counter", COMPONENT.as_bytes(), SIGNATURE.as_bytes())
        .unwrap();

    let replacement = COMPONENT.replace("i32.const 42", "i32.const 99");
    let err = plugin
        .reload_signed(replacement.as_bytes(), SIGNATURE.as_bytes())
        .expect_err("the replacement is not what was signed");
    assert_eq!(err.kind(), ErrorKind::SignatureInvalid);

    // And the running plugin is untouched — refused before it was entered.
    assert_eq!(answer(&mut plugin), 42);
    assert_eq!(plugin.stats().reloads, 0);
}

#[test]
fn an_unsigned_reload_cannot_slip_past_a_signed_load() {
    let host = host_trusting(&[SIGNER]);
    let mut plugin = host
        .load_binary_signed("counter", COMPONENT.as_bytes(), SIGNATURE.as_bytes())
        .unwrap();

    let err = plugin
        .reload(COMPONENT.as_bytes())
        .expect_err("reload with no signature at all");
    assert_eq!(err.kind(), ErrorKind::SignatureInvalid);
    assert_eq!(plugin.stats().reloads, 0);
}

#[test]
fn a_key_that_will_not_parse_fails_the_host_and_not_the_first_plugin() {
    // A broken manifest is a configuration mistake. Reporting it on the first
    // load would dress it up as a plugin problem.
    let err = Host::builder()
        .manifest(Manifest::parse("[signature]\nkeys = [\"nonsense\"]\n").unwrap())
        .build()
        .expect_err("that is not a PEM public key");
    assert_eq!(err.kind(), ErrorKind::Manifest);
    assert!(
        err.message().contains("signature.keys[0]"),
        "{}",
        err.message()
    );
}

// ---------------------------------------------------------------------------
// A policy file must say whether plugins have to be signed.

fn write_policy(dir: &std::path::Path, toml: &str) -> PathBuf {
    let path = dir.join("policy.toml");
    std::fs::write(&path, toml).unwrap();
    path
}

#[test]
fn a_policy_file_that_says_nothing_about_signing_is_refused() {
    // The whole point: "nobody thought about it" and "we decided not to" must
    // not look the same in a file someone reviews before installing a plugin.
    let dir = tempfile::tempdir().unwrap();
    let path = write_policy(dir.path(), "[permissions]\nrandom = true\n");

    let err = Manifest::from_file(&path).expect_err("no signature posture stated");
    assert_eq!(err.kind(), ErrorKind::Manifest);
    // It has to say what to write, or it is just an obstacle.
    assert!(
        err.message().contains("required = false"),
        "{}",
        err.message()
    );
    assert!(err.message().contains("keys"), "{}", err.message());
    // And name the file, since a host may load several.
    assert!(err.message().contains("policy.toml"), "{}", err.message());
}

#[test]
fn an_explicit_opt_out_is_accepted_and_verifies_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_policy(
        dir.path(),
        "[permissions]\nrandom = true\n\n[signature]\nrequired = false\n",
    );

    let manifest = Manifest::from_file(&path).expect("the file states its posture");
    assert!(!manifest.signature.is_required());
    assert!(manifest.signature.states_a_posture());
}

#[test]
fn keys_alone_state_the_posture_without_a_required_flag() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_policy(
        dir.path(),
        &format!(
            "[signature]\nkeys = [\"\"\"\n{}\"\"\"]\n",
            SIGNER.trim_start()
        ),
    );

    let manifest = Manifest::from_file(&path).expect("listing a key is stating a posture");
    assert!(manifest.signature.is_required());
}

#[test]
fn a_contradictory_policy_is_refused_rather_than_silently_resolved() {
    // Keys listed *and* opted out. Picking a winner would mean guessing which
    // half the author meant, and guessing wrong is either a false sense of
    // verification or an unexplained refusal.
    let dir = tempfile::tempdir().unwrap();
    let path = write_policy(
        dir.path(),
        &format!(
            "[signature]\nrequired = false\nkeys = [\"\"\"\n{}\"\"\"]\n",
            SIGNER.trim_start()
        ),
    );

    let err = Manifest::from_file(&path).expect_err("required = false contradicts keys");
    assert!(err.message().contains("contradicts"), "{}", err.message());
}

#[test]
fn requiring_signatures_with_no_keys_is_refused_because_nothing_could_load() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_policy(dir.path(), "[signature]\nrequired = true\n");

    let err = Manifest::from_file(&path).expect_err("required, but nothing to verify against");
    assert!(
        err.message().contains("no plugin could ever load"),
        "{}",
        err.message()
    );
}

#[test]
fn parsing_a_string_is_deliberately_not_subject_to_the_rule() {
    // Load-bearing asymmetry, not an oversight. A recorded trace carries its
    // manifest as TOML and replays by parsing it back, so enforcing this in
    // `parse` would make every trace recorded before the rule existed
    // unreplayable — and a bug report matters most once something has broken.
    let manifest = Manifest::parse("[permissions]\nrandom = true\n")
        .expect("inline manifests stay permissive");
    assert!(!manifest.signature.states_a_posture());
}

#[test]
fn every_shipped_policy_states_a_signature_posture() {
    // These are the files the rule exists for, and they are also what people
    // copy when writing their first policy.
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/policies");
    let mut checked = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "toml") {
            continue;
        }
        Manifest::from_file(&path)
            .unwrap_or_else(|err| panic!("{}: {}", path.display(), err.message()));
        checked += 1;
    }
    assert_eq!(checked, 8, "expected eight shipped policies");
}
