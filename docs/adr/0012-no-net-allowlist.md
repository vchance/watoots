# ADR-0012 — There is no `permissions.net` host allowlist, because there cannot be one here

Date: 2026-09-06. Status: accepted. Supersedes the `net` allowlist shape
described in `docs/MANIFEST.md` up to 0.2.0.

## Context

`permissions.net` has been a list of hostnames since the first manifest:

```toml
[permissions]
net = ["api.example.com"]
```

It has never worked. A non-empty list was refused at host-build time with "not
enforced yet", so the only two reachable states were `net` absent (deny) and
`net = []` (grant the imports, reach nothing). The list shape existed as a
promise about a future release.

The promise cannot be kept at this layer, and finding out why is what prompted
this ADR. `wasmtime-wasi`'s hook for outbound connections is
`WasiCtxBuilder::socket_addr_check`, and it is handed a resolved
[`SocketAddr`](https://doc.rust-lang.org/std/net/enum.SocketAddr.html) — an IP
and a port. The hostname is gone by then: the guest called
`wasi:sockets/ip-name-lookup` (or its runtime did), got addresses back, and
connects to an address. There is no seam between those two steps that watoots
can reach, because the lookup and the connect are separate guest-initiated
calls with the guest's own code in between.

So a rule about `api.example.com` has nothing to match on. The options for
honouring one are all bad:

- **Resolve the name at load and pin the addresses.** DNS is not stable. A CDN
  hands back different addresses per query, per region, per minute. The
  allowlist would deny legitimate traffic constantly, and — worse — an address
  that was `api.example.com` at load could belong to someone else by the time
  the plugin connects.
- **Intercept `ip-name-lookup` and only return addresses for allowed names.**
  It does not hold even in principle: the guest can skip the lookup and connect
  to a literal address. It restricts the polite plugin and not the hostile one,
  which is the wrong way round for a sandbox. (In wasmtime-wasi 48 the question
  is moot — `AllowedNetworkUses` derives `Default`, so `ip_name_lookup` is
  `false` and name resolution is denied outright unless an embedder opts in.
  watoots does not.)
- **Ship the list unenforced with a warning.** This is what we had.

## Decision

**Remove the list shape. `net` grants the socket imports or denies them.**

```toml
[permissions]
net = "linked"   # wasi:sockets and wasi:http may be imported; nothing is reachable
net = "deny"     # the imports fail to load. The default, same as omitting the key
```

A manifest key that looks like a restriction and enforces nothing is worse than
no key at all. Someone reviewing a policy before installing a plugin — the whole
point of `docs/MANIFEST.md` — reads `net = ["api.example.com"]` and concludes
the plugin can reach one host. Under every option above, that conclusion is
wrong in the direction that hurts. The refusal at host-build time protected the
person who tried to *use* it and not the person who tried to *read* it.

Two values rather than a boolean, deliberately. `net = true` would read as
"network allowed", which is precisely the false conclusion this ADR exists to
prevent. `"linked"` claims only what it does: the interfaces resolve.

### The state that survives is the one guests forced on us

`net = "linked"` is not a convenience. CPython links `wasi:sockets`
unconditionally and a JavaScript runtime links `wasi:http`, whether or not the
plugin author ever opens a socket. Denying the import refuses those languages
outright. Granting it costs nothing, because wasmtime-wasi refuses every
connection unless the embedder installs a check and watoots installs none — we
never call `allow_tcp`, `allow_udp` or `socket_addr_check`.

That last sentence is the actual security property, and it is stronger than any
allowlist we could have shipped: not "connections are filtered" but "connections
do not succeed". Verified at the source rather than assumed —
`wasmtime-wasi` 48.0.1's `AllowedNetworkUses` derives `Default`, so `tcp`, `udp`
and `ip_name_lookup` are all `false`, and every opt-in is a builder call watoots
never makes.

It does rest on those defaults. A future wasmtime-wasi that flipped one would
change what `"linked"` means without changing a line here, so the claim is worth
re-checking at an engine bump — which is already an ADR-gated event.

### Where hostname policy does belong

With whatever the application serves behind `wasi:http`. An embedder that wants
a plugin to reach `api.example.com` and nothing else implements
`wasi:http/outgoing-handler` itself and applies the rule there, where the
request still carries its authority. That host function sees a URL. We see a
`SocketAddr`. The rule belongs where the name is.

`Host::builder().host_func` and `provide_interface` already exist for exactly
this shape, so this is a redirection rather than a gap.

## Consequences

- **A breaking manifest change**, and the first one that invalidates a file
  people may have written. `net = []` and `net = ["host"]` both become parse
  errors. The error names `net = "linked"` as the replacement rather than
  reporting a serde type mismatch, because the error is the only place a reader
  with an old manifest finds out what to do.
- `Permissions::net` changes from `Option<Vec<String>>` to `NetGrant`, so it is
  a source-breaking Rust change too. `is_granted()` replaces `is_some()`.
- The host-build refusal disappears: there is no longer an unenforceable
  manifest to refuse. `Manifest::parse` rejects the old shape instead, which
  moves the error to where the mistake is.
- The C API is unaffected — `net` never crossed it as a value, only as the grant
  name `"permissions.net"` in a denial.
- `docs/SECURITY.md` gains a "will not be one at this layer" rather than a
  "comes later". A known gap that is never going to close should not be filed
  as a gap.
- If wasmtime-wasi ever routes name lookups and connections through one hook
  that keeps the authority, this is the ADR to reopen. It would need to survive
  the literal-address bypass to be worth anything.
