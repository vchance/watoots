# ADR-0014 — Verify a pinned-key signature at load; leave identity and transparency to whoever fetched the bytes

Date: 2026-09-06. Status: accepted.

## Context

A manifest says what a plugin may *do*. Nothing says who it came *from*. That
is the gap: `docs/MANIFEST.md` is sold as the file you read before installing a
plugin, and it cannot currently express "and only if it was published by us".

The September 2026 landscape survey found signature verification at load to be
genuinely unfilled — WASI itself publishes Sigstore provenance that essentially
nothing verifies at the point of execution. So the question is not whether it is
useful, but which part of it watoots can actually enforce. ADR-0012 removed a
manifest key that looked like a restriction and enforced nothing; this ADR has
to avoid adding one.

### What exists, checked

| | |
|---|---|
| `wasm-signatures/design` — the "WebAssembly module signatures" proposal | last push **2022-05-18**, 18 stars |
| `wasmsign2` — its one implementation, embeds a signature in a custom section | 0.2.7, last release 2025-12-29, ~832 recent downloads |
| `sigstore-rs` | 0.14.0, updated 2026-05-22, ~308k recent downloads, self-described "experimental" |
| `cosign sign-blob` | default algorithm `ecdsa-sha2-256-nistp256`; verifies against a PEM public key; offline verification supported |

The embedded-signature route is a dormant proposal with one low-adoption
implementation. Betting the format on it would be inventing-by-proxy. The
ecosystem is on Sigstore: wasmCloud documents signing components with cosign,
and current guidance is to use OCI as the signing surface.

### Two very different things get called "signature verification"

1. **Does this byte string carry a valid signature by a key I already trust?**
   Pure computation. No network, no clock, no trust root beyond the key the
   host was configured with.
2. **Was this published by a particular identity, and is that attested in a
   transparency log?** Sigstore keyless. Needs Fulcio, Rekor, a trust root that
   must be kept fresh, and network access — at load.

Only the first is enforceable where watoots sits.

## Decision

**Verify a detached, pinned-key signature over the component bytes, on the
single load path. Do not attempt keyless identity or transparency-log
verification.**

The manifest names trusted public keys. A component whose bytes do not verify
against one of them does not load, is not compiled, and is not cached.

### Why here and not in the application

The application could hash and check the bytes before calling `Host::load`, and
for a first load that is a fine answer. Three things make the load path the
right home anyway, and the third is decisive:

- **The manifest is the reviewable artifact.** Its value is that one file
  answers "what is this plugin allowed to be". A publisher constraint belongs
  beside the capability constraints, read at the same moment by the same person.
- **Load is the only chokepoint.** Verifying "at download" leaves every other
  route to `load_binary` unverified. Verifying at load means no unverified bytes
  are ever compiled, cached, or instantiated, because there is one function they
  all pass through.
- **Reload.** ADR-0010 established that reload re-runs the grant check, because
  a plugin must not acquire a capability by being updated. A signature check
  outside watoots is a check that reload silently skips — and the moment the
  code changes is exactly the moment publisher identity matters most. Putting
  verification on the shared path makes "reload re-verifies" true by
  construction rather than by the application remembering.

### The format is cosign's, because a signing tool has to already exist

`cosign sign-blob --key cosign.key --output-signature plugin.wasm.sig
plugin.wasm` produces the signature; the manifest carries the corresponding
public key. That is ECDSA P-256 over SHA-256, which is cosign's default, and
verifying it needs no registry, no Rekor and no network.

Choosing an existing tool's output over a bespoke scheme is the same reasoning
ADR-0006 used to refuse inventing a metrics interface: the value is in the
verification, and a format nobody else can produce has none.

### What this deliberately does not do

- **No keyless / OIDC / Fulcio / Rekor.** It would put a network fetch and a
  refreshable trust root inside `Host::load`, in a library whose entire pitch is
  a sandbox that does not reach the network. An application that wants identity
  attestation should verify the bundle when it *fetches* the plugin, and then
  hand watoots bytes plus a pinned key. `docs/SECURITY.md` has to say this
  plainly, because a reader who assumes otherwise assumes it in the direction
  that hurts.
- **No key distribution, rotation, or revocation.** The manifest pins keys. Who
  rotates them, and how a compromised key is withdrawn, is the deployment's
  problem — and pretending otherwise would be ADR-0012's mistake again.
- **No trust on first use.** A missing signature under a manifest that requires
  one is a refusal, never a prompt and never a warning.

## Consequences

- `Manifest` grows a section for trusted keys, and it is absent-means-off:
  unlike every capability, the default cannot be deny. A host that has not
  configured keys must keep loading unsigned plugins or every existing embedder
  breaks on upgrade. That is a real asymmetry with the rest of the manifest and
  `docs/MANIFEST.md` must call it out rather than let the "absent denies" rule
  be assumed to apply here.
- A new dependency tree for ECDSA. It is RustCrypto, the same family as the
  `sha2` already in the graph.
- Verification happens before compilation, so a bad signature costs a hash and a
  curve operation rather than a compile.
- The audit trail gains the event that makes it complete: it already records
  which bytes were loaded, and can now record whether they were signed and by
  which key.
- A signed plugin's `.wasm` is unchanged, so nothing about the cache key, the
  trace format or `wasm-tools` interop moves. That is the practical advantage of
  detached over embedded, independent of the proposal's health.
- **`replay` and `fuzz` clear the signature policy, and that had to be
  decided rather than discovered.** A trace carries the manifest but not the
  signature, so a recording made under a `[signature]` policy would be
  unreplayable — and a bug report matters most exactly when something has
  stopped working. `watoots fuzz` is the same shape: a component the operator
  named, run repeatedly to find a crash. Both keep every permission and limit
  and drop only "who published this", which the person running them answered by
  choosing the file. It is a real weakening and so it is written here, in
  `docs/MANIFEST.md`, and in a comment at both sites, rather than left as a
  surprise for whoever finds it.
- **If the module-signatures proposal ever revives**, embedded signatures become
  worth supporting alongside this — the verification core would be reused and
  only the "where does the signature come from" step changes. This ADR is the
  place that says so.
