# Signing fixtures

Real artifacts, produced by `openssl` — not by watoots. Verifying something this
crate also signed would prove only that it agrees with itself; the point of
ADR-0014 is interoperating with what `cosign sign-blob` writes, and openssl
produces the same ECDSA-P256-over-SHA-256 that cosign defaults to.

```sh
openssl ecparam -name prime256v1 -genkey -noout -out signer.key.pem
openssl ec -in signer.key.pem -pubout -out signer.pub.pem

# The signature covers component.wat byte for byte, including its final newline.
openssl dgst -sha256 -sign signer.key.pem -out component.der component.wat
openssl base64 -A -in component.der -out component.wat.sig

# A second, unrelated key. Its private half was discarded: it exists only to be
# a key that did not sign this, which is the case a checksum cannot detect.
openssl ecparam -name prime256v1 -genkey -noout -out stranger.key.pem
openssl ec -in stranger.key.pem -pubout -out stranger.pub.pem
```

The private keys are deliberately **not** here. Nothing in the suite needs to
sign anything, and a private key in a public repository is a bad habit even when
it guards nothing.

`component.wat.sig` is named for the convention `Host::load` follows: the
signature for `plugin.wasm` lives at `plugin.wasm.sig`, which is where
`cosign sign-blob --output-signature` is normally pointed.
