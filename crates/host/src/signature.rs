//! Verifying that a component was signed by a key the manifest trusts.
//!
//! Scope is ADR-0014: a detached signature over the component bytes, checked
//! against public keys pinned in the manifest. No network, no transparency log,
//! no identity — those need Fulcio, Rekor and a trust root that has to be kept
//! fresh, and none of that belongs inside `Host::load`.
//!
//! The format is what `cosign sign-blob` produces, because the verification is
//! only worth anything if a signing tool already exists:
//!
//! ```text
//! cosign sign-blob --key cosign.key --output-signature plugin.wasm.sig plugin.wasm
//! ```
//!
//! which is ECDSA P-256 over SHA-256, base64-encoded. `openssl` produces the
//! same bytes, and the tests use it precisely so that what is verified here is
//! an *external* artifact rather than something this module also generated.

use base64ct::{Base64, Encoding};
use p256::ecdsa::signature::Verifier;
use p256::ecdsa::{Signature, VerifyingKey};
use p256::pkcs8::DecodePublicKey;

use crate::{Error, ErrorKind, Result};

/// A public key a plugin may be signed with, as it appeared in the manifest.
#[derive(Debug, Clone)]
pub struct TrustedKey {
    /// Where it sat in `signature.keys`, so a refusal can name it without
    /// printing key material.
    index: usize,
    key: VerifyingKey,
}

impl TrustedKey {
    /// Parse one PEM `SUBJECT PUBLIC KEY INFO` block — a `cosign.pub`, or the
    /// output of `openssl ec -pubout`.
    fn parse(index: usize, pem: &str) -> Result<Self> {
        let key = VerifyingKey::from_public_key_pem(pem.trim()).map_err(|err| {
            Error::new(
                ErrorKind::Manifest,
                format!(
                    "signature.keys[{index}] is not a PEM public key: {err}. Expected a \
                     `-----BEGIN PUBLIC KEY-----` block holding a P-256 key, which is what \
                     `cosign public-key` and `openssl ec -pubout` write"
                ),
            )
        })?;
        Ok(Self { index, key })
    }

    /// Which entry of `signature.keys` this is.
    #[must_use]
    pub fn index(&self) -> usize {
        self.index
    }
}

/// Parse every key in a manifest, failing on the first one that will not parse.
///
/// Done once when the host is built rather than per load: a key that cannot be
/// parsed is a broken manifest, and finding that out on the first *load* would
/// report a configuration error as a plugin problem.
pub(crate) fn parse_keys(pems: &[String]) -> Result<Vec<TrustedKey>> {
    pems.iter()
        .enumerate()
        .map(|(index, pem)| TrustedKey::parse(index, pem))
        .collect()
}

/// Check `signature` against every trusted key, returning the one that matched.
///
/// Every key is tried because a manifest lists more than one exactly when it is
/// mid-rotation. The signature is base64 as `cosign sign-blob
/// --output-signature` writes it, wrapping an ASN.1 DER `(r, s)` pair.
pub(crate) fn verify<'a>(
    wasm: &[u8],
    signature: &[u8],
    keys: &'a [TrustedKey],
) -> Result<&'a TrustedKey> {
    debug_assert!(!keys.is_empty(), "callers check this and report it better");

    let text = std::str::from_utf8(signature)
        .map_err(|_| refused("the signature is not text; expected base64 as cosign writes it"))?;
    let der = Base64::decode_vec(text.trim()).map_err(|err| {
        refused(&format!(
            "the signature is not valid base64 ({err}); expected the contents of \
             `cosign sign-blob --output-signature`"
        ))
    })?;
    let parsed = Signature::from_der(&der).map_err(|err| {
        refused(&format!(
            "the signature is not a DER ECDSA signature: {err}"
        ))
    })?;

    // No short-circuit on the *first* key: `verify` is constant-time per key,
    // and a caller learning which key failed first tells them nothing they
    // could not learn by trying each themselves.
    keys.iter()
        .find(|trusted| trusted.key.verify(wasm, &parsed).is_ok())
        .ok_or_else(|| {
            refused(&format!(
                "no trusted key verifies it ({} tried). The plugin was not signed by anyone \
                 this manifest trusts, or the bytes have changed since it was signed",
                keys.len()
            ))
        })
}

fn refused(detail: &str) -> Error {
    Error::new(
        ErrorKind::SignatureInvalid,
        format!("signature verification failed: {detail}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // Generated with openssl, which produces the same ECDSA-P256-over-SHA-256
    // that `cosign sign-blob` does by default:
    //
    //   openssl ecparam -name prime256v1 -genkey -noout -out key.pem
    //   openssl ec -in key.pem -pubout -out pub.pem
    //   openssl dgst -sha256 -sign key.pem -out sig.der component.wat
    //   openssl base64 -A -in sig.der
    //
    // Embedded rather than generated at test time so the suite stays hermetic,
    // and deliberately *external*: verifying something this module also signed
    // would prove only that it agrees with itself.
    const PUBLIC_KEY: &str = "\
-----BEGIN PUBLIC KEY-----
MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEzuhbtxgdBr7pXOlkrACLK8+PXkOL
WmxJSG+8X0cSE6TaYhzhil1GgjEIbsI9QoDGb1DR6/EBuxvg2E4ejBuqDg==
-----END PUBLIC KEY-----
";

    /// A different key, to prove a valid signature by the wrong signer fails.
    const OTHER_KEY: &str = "\
-----BEGIN PUBLIC KEY-----
MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEcQXQoicfb86CIKwLPhu2brdggQ5J
Aa3FlXUmXZyojIaVeo+wZN+6Pyj0Uxsvu4S81xmnKtSLLW2NA+7jS0XWOA==
-----END PUBLIC KEY-----
";

    const SIGNED_BYTES: &str = r#"(component
  (core module $m (func (export "answer") (result i32) (i32.const 42)))
  (core instance $i (instantiate $m))
  (func $answer (result s32) (canon lift (core func $i "answer")))
  (export "answer" (func $answer))
)
"#;

    const SIGNATURE: &str = "MEYCIQCiA/lOo1PQR7dSStVRyxESjOlphTIzC8KlGedDiNTnhwIhALtySx9SgAmLcIcvNhWqZkQEG/S1G14rgSgS3mwMk+8I";

    fn keys(pems: &[&str]) -> Vec<TrustedKey> {
        parse_keys(&pems.iter().map(|s| (*s).to_string()).collect::<Vec<_>>()).unwrap()
    }

    #[test]
    fn a_signature_made_by_openssl_verifies() {
        let trusted = keys(&[PUBLIC_KEY]);
        let matched = verify(SIGNED_BYTES.as_bytes(), SIGNATURE.as_bytes(), &trusted)
            .expect("openssl signed exactly these bytes with exactly this key");
        assert_eq!(matched.index(), 0);
    }

    #[test]
    fn one_changed_byte_fails() {
        let trusted = keys(&[PUBLIC_KEY]);
        let mut tampered = SIGNED_BYTES.as_bytes().to_vec();
        // 42 becomes 43: a plausible edit, not a corrupted file.
        let at = SIGNED_BYTES.find("42").unwrap();
        tampered[at + 1] = b'3';
        let err = verify(&tampered, SIGNATURE.as_bytes(), &trusted).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::SignatureInvalid);
        assert!(
            err.message().contains("no trusted key"),
            "{}",
            err.message()
        );
    }

    #[test]
    fn a_real_signature_by_an_untrusted_key_fails() {
        // The signature is valid. The signer is not one we trust, which is the
        // whole point: this is the attack a checksum does not stop.
        let trusted = keys(&[OTHER_KEY]);
        let err = verify(SIGNED_BYTES.as_bytes(), SIGNATURE.as_bytes(), &trusted).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::SignatureInvalid);
    }

    #[test]
    fn any_listed_key_may_be_the_signer_because_that_is_what_rotation_needs() {
        let trusted = keys(&[OTHER_KEY, PUBLIC_KEY]);
        let matched = verify(SIGNED_BYTES.as_bytes(), SIGNATURE.as_bytes(), &trusted).unwrap();
        assert_eq!(matched.index(), 1, "the second key is the signer");
    }

    #[test]
    fn trailing_whitespace_in_a_signature_file_is_not_an_error() {
        // `cosign ... --output-signature sig` then `cat sig` in a shell script
        // is how these travel, and something always adds a newline.
        let trusted = keys(&[PUBLIC_KEY]);
        let padded = format!("{SIGNATURE}\n");
        verify(SIGNED_BYTES.as_bytes(), padded.as_bytes(), &trusted).unwrap();
    }

    #[test]
    fn a_signature_that_is_not_base64_says_so_rather_than_failing_the_check() {
        // Handing over the raw DER instead of cosign's base64 is the obvious
        // mistake, and it must not read as "wrong signer".
        let trusted = keys(&[PUBLIC_KEY]);
        let der = [0x30, 0x46, 0x02, 0x21, 0x00];
        let err = verify(SIGNED_BYTES.as_bytes(), &der, &trusted).unwrap_err();
        assert!(
            err.message().contains("base64") || err.message().contains("not text"),
            "{}",
            err.message()
        );
    }

    #[test]
    fn a_key_that_is_not_a_pem_public_key_is_a_manifest_error() {
        let err = parse_keys(&["not a key".to_string()]).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Manifest);
        assert!(
            err.message().contains("signature.keys[0]"),
            "{}",
            err.message()
        );
    }
}
