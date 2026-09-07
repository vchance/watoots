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
