# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **[docs/WRITING-A-PLUGIN.md](docs/WRITING-A-PLUGIN.md)** — a plugin from
  nothing to running under a policy, in Rust, in about fifteen minutes. Every
  command was run in order and its output pasted back rather than written from
  memory. It leads with the thing that surprises people: a six-line plugin that
  formats a string still wants the clock and the environment, because `std`
  links them, and `watoots inspect` shows you that before it runs.

- **[docs/PERFORMANCE.md](docs/PERFORMANCE.md)** and
  `cargo bench -p watoots --bench boundary` — what the boundary costs, measured.
  A crossing is ~330 ns and `[limits]` adds nothing measurable to it; the cost
  of fuel and deadlines is per *instruction*, roughly 2.0x and 1.7x on a tight
  loop, which is close to their worst case. WAVE adds ~240 ns to a small call,
  and the compiled-component cache saves ~130 µs per instance after the first.

  The document says plainly what the measurement cannot support as well as what
  it can: in the per-crossing group fuel appears *faster* than no limits, which
  is not possible. A `control/` group settles why — two hosts built from
  identical manifests differ by 11 ns, and swapping their order moves the
  slowness with the position rather than the host, so the first benchmark in a
  group pays a warmup criterion does not absorb. That group therefore resolves
  to "about 330 ns" and nothing finer, while the metering gaps (µs on ns bias)
  survive easily. Not a CI gate — microbenchmarks on shared runners are noise.

### Changed

- **Wasmtime 48.0.1 → 48.0.2.** No security fix in it — it vendors
  `cap-primitives` into Wasmtime and fixes `bindgen!` output against a Rust
  nightly — but the point of pinning an LTS line is taking its patches rather
  than judging each one. No advisory has been published against Wasmtime since
  2026-08-20, and both of those were fixed in 48.0.0.

  ADR-0012 and ADR-0013 each asked for a claim to be re-checked at an engine
  bump, and both were: the p3 module still states that security fixes limited to
  wasip3 get no patch release, and `AllowedNetworkUses` still derives `Default`,
  so no socket use is enabled unless an embedder opts in.

## [0.5.0] — 2026-09-08

**One breaking manifest change and it is the point of the release:** a policy
file must now state whether plugins have to be signed. 0.4.0 shipped signature
verification off by default, so a deployment could end up unverified because
nobody considered it — an opt-in security control nobody opts into protects
nobody. Anyone on 0.4.0 should move.

Also breaking for Rust callers: `GrantReport::summarize` takes `&Manifest`
rather than `&Permissions`.

### Changed

- **A policy file must now state whether plugins have to be signed.** Omit
  `[signature]` from a file and it does not parse; the error says to add `keys`
  or `required = false`. Breaking for any policy file written against 0.4.0.

  0.4.0 shipped signing off-by-default, which meant a deployment could end up
  unverified because nobody considered it — and an opt-in security control that
  nobody opts into protects nobody. There is now no default: "we did not think
  about signing" and "we decided not to" cannot look the same in a file someone
  reviews before installing a plugin. A contradictory policy (`required = false`
  alongside `keys`) and an unsatisfiable one (`required = true` with no keys)
  are refused rather than silently resolved.

  **`Manifest::parse` is deliberately exempt** — only `Manifest::from_file`
  enforces it. A recorded trace carries its manifest as TOML and replays by
  parsing it back, so enforcing this in `parse` would make every trace recorded
  before this change unreplayable, and a bug report matters most once something
  has already broken. Inline manifests are covered by the warning below, which
  no construction path can dodge.

- **Running unsigned is loud.** `watoots run`, `record`, `profile` and `reload`
  print a warning to stderr naming the risk in plain terms — the sandbox holds,
  but anyone able to replace the file gets everything the policy grants, and
  nothing can tell a replacement from the original. Not behind `--audit`: a
  warning nobody sees by default is not a control.

### Added

- **`watoots inspect` reports the signature posture**, in a `publisher` section
  beside the capability table. It listed every permission and said nothing about
  who may have signed the plugin, which is incomplete in the one section a
  reviewer most needs — and `inspect` is the read-this-before-you-install tool.
  It names the consequence rather than the setting: *not verified — any bytes at
  this path load, with everything granted above*.

  `GrantReport::summarize` now takes `&Manifest` rather than `&Permissions`,
  since the report is about the whole policy and not one section of it.

- `AuditEvent::LoadedUnverified` / `WT_AUDIT_LOADED_UNVERIFIED` (8), emitted on
  every load and reload under a manifest with no trusted keys. It carries the
  plugin name and digest and spells out the risk in its rendered line, so the
  fact that nobody checked the publisher is in the record and not only on
  screen.
- `SignaturePolicy::required` and `SignaturePolicy::states_a_posture()`, plus
  `SignaturePolicy::validate()` for callers that build a manifest themselves and
  want the same rule.

## [0.4.0] — 2026-09-07

**One breaking manifest change:** `permissions.net` no longer accepts a list, so
a manifest that parsed under 0.3.0 can fail under this one. That is why this is
0.4.0 and not a patch. See *Removed* for what to write instead; the parse error
says it too.

**One security fix**, in a capability the manifest could already express: see
*Fixed (security)*.

**C ABI:** additive only, and `wt_status` gained
`WT_ERR_SIGNATURE_INVALID = 9` appended, so no existing enumerator value moved.
Three new functions — `wt_host_load_binary_signed`, `wt_plugin_reload_signed`,
`wt_host_builder_audit_hook` — and five new types for the audit trail:
`wt_audit_hook_t`, `wt_audit_event_t`, `wt_audit_kind`, `wt_audit_verdict`,
`wt_ceiling`. A caller compiled against the 0.3.0 header keeps working; one
compiled against this header needs this header. Nothing is published to
crates.io, so nobody is holding a stale one, but it is an ABI change and this is
where it is said.

### Added

- **Signature verification at load** ([ADR-0014](docs/adr/0014-signature-verification.md)).
  A `[signature]` section names public keys, and a component not signed by one
  of them does not load, is not compiled, and is not cached. The format is what
  `cosign sign-blob` writes — ECDSA P-256 over SHA-256, base64 — so a signing
  tool already exists. `Host::load` reads `<plugin>.wasm.sig` from beside the
  component; `load_binary_signed` / `wt_host_load_binary_signed` take it as an
  argument. **Reload re-verifies**, which is the reason this lives in watoots
  rather than in the application: a check made before `load` is one that reload
  silently skips.

  Pinned keys only. No Sigstore keyless identity, no certificate chains, no
  transparency-log inclusion — those need network access and a maintained trust
  root inside `Host::load`. Verify a bundle where you fetch the plugin instead.

  **`[signature]` is the one part of a manifest where absence does not deny**,
  because otherwise upgrading would stop every existing plugin from loading.
  `watoots replay` and `watoots fuzz` also skip the check, since a trace carries
  the manifest but not the signature.

- **An audit trail** ([ADR-0011](docs/adr/0011-audit-trail.md)). `AuditHook` is a
  fourth observer carrying authorisation decisions only — never argument values,
  so an audit line is safe to keep when a trace is not. Eight events: a plugin
  loaded or refused, each import's verdict, a reload allowed or refused, a log
  line admitted or suppressed by the level ceiling, and a ceiling spent. Six
  ceilings, including `Memory`, whose refusal previously reached *nobody*: the
  store limiter says no, `memory.grow` answers `-1`, and the call carries on.

  Off unless installed — a library writing to stderr uninvited is badly behaved.
  `watoots run --audit` and its siblings turn it on; `wt_host_builder_audit_hook`
  is the C surface.

- **`watoots diff old.wasm new.wasm`** — what changed between two builds, and
  whether the update would still load. `Plugin::reload` already re-runs the
  grant check and refuses a replacement that wants more (ADR-0010); this is that
  refusal previewed, before the update ships. New imports are reported with the
  manifest key that would grant them, removed exports as breaking callers, and
  the two are counted separately because they are fixed in different places.
  Exits non-zero on either.

  Not a reimplementation of `wasm-tools component semver-check`, which compares
  two WIT *packages* and which watoots wraps as `watoots wit semver-check`
  (ADR-0007). This compares two compiled *components* against a policy.

- **Compiled components are reused across loads.** A `Host` keeps the
  `Component` it built, keyed by content hash, so a second instance of the same
  plugin costs a clone rather than a compile — the normal shape of a plugin host
  is many instances of one component, and without this each paid a full compile
  (seconds, for an 18MB interpreter guest). Bounded at 32 distinct components,
  oldest evicted, so a host reloading on every file change cannot accumulate a
  morning's builds. `Host::compiles()` reports how many were actually built, so
  a test can tell reuse from a rebuild.

- `ErrorKind::SignatureInvalid` / `WT_ERR_SIGNATURE_INVALID` (9), distinct from
  `PermissionDenied`: one means the plugin asked for something it was not
  granted, the other that the bytes are not from a publisher you trust, and they
  send a reader to different halves of the manifest.

### Removed

- **The `permissions.net` host allowlist, which never worked and could not**
  ([ADR-0012](docs/adr/0012-no-net-allowlist.md)). `net` is now `"deny"` (the
  default) or `"linked"`, and never a list. A non-empty list was already refused
  at host-build time as "not enforced yet"; the promise cannot be kept at this
  layer, because `wasmtime-wasi`'s `socket_addr_check` receives a resolved
  `SocketAddr` and never the hostname that produced it. A manifest key that
  reads as a restriction and enforces nothing is worse than no key — it misleads
  the person reviewing a policy before installing a plugin, which is what the
  manifest is *for*.

  **Migrating:** `net = []` becomes `net = "linked"`; drop any named hosts and
  apply the rule in your own `wasi:http` implementation, where the name still
  exists. `net = ["example.com"]` never granted `example.com` in any released
  version. `Permissions::net` is now a `NetGrant` rather than an
  `Option<Vec<String>>`, and `is_granted()` replaces `is_some()`. The old shape
  is a parse error that names `net = "linked"` as the replacement.

### Fixed (security)

- **WASI 0.3's wall clock would have been classified as the monotonic one.**
  0.3 renames `wasi:clocks/wall-clock` to `system-clock` and adds `timezone`;
  the capability table matched `wall-clock` by name and fell through on
  `("clocks", _)` to `MonotonicClock`. A manifest saying `clocks = "monotonic"`
  — the setting chosen precisely to keep real time away from a plugin — would
  have admitted the real-time clock. `clocks` is the only WASI package split
  across two different capabilities, which is why a fallthrough there
  over-grants where the `filesystem` and `sockets` ones cannot. The arm is now
  exhaustive and an unread `clocks` interface denies.

  Latent rather than exploitable: the host links wasip2 only, so a p3 component
  fails at instantiation either way. But classification runs first, so
  `watoots inspect` would have reported a p3 plugin as satisfying a
  monotonic-only policy. Found by writing ADR-0013.

- **A new `ErrorKind` reached C as "a bug on our side".** `From<ErrorKind> for
  wt_status` needs a catch-all because `ErrorKind` is `#[non_exhaustive]`, and
  it silently mapped `SignatureInvalid` to `WT_ERR_INTERNAL` with nothing
  failing to compile. `every_error_kind_has_its_own_status` now fails when a
  kind falls through, since the compiler cannot.

### Changed

- **A written WASI p3 position** ([ADR-0013](docs/adr/0013-wasi-p3-position.md)),
  replacing an open question in the spec. The host stays on 0.2, and the two
  conditions for adopting p3 are written down and checkable. The disqualifying
  one is that `wasmtime-wasi`'s p3 module states security fixes limited to
  wasip3 get no patch release, which a sandbox cannot depend on. Two claims in
  `docs/SPEC.md` were re-checked and corrected: `wasm32-wasip3` is still Tier 3
  in the rustc book (the promotion proposal was accepted, which is not the
  same), and it is Spin 4.1.0 rather than 4.0 that ships p3 on Wasmtime 48.

- "The permission model is 0.3-shaped" is now a test rather than an intention:
  `crates/host/tests/wasip3_shape.rs` classifies the p3 interface set read from
  `wasmtime-wasi` 48.0.1's own WIT.

- **CI checks the MSRV.** `rust-version` is a promise to anyone depending on
  watoots and every other job runs on stable, so a dependency bump could raise
  the real floor with nobody noticing. The job reads the version out of
  `Cargo.toml`, so the two cannot drift.

## [0.3.0] — 2026-09-06

Additive for manifests and plugins. **One C ABI change:** `wt_plugin_stats_t`
gained a trailing `reloads` field, so a caller compiled against a 0.2.0 header
reads a struct one field short. Nothing is published to crates.io, so nobody is
holding a stale header — but it is an ABI change and this is where it is said.

### Added

- **A second example world, and four guests for it.** `examples/wit/asset` is an
  image pipeline: a record carrying a large `list<u8>`, a `variant` with
  payloads, `result<image, failure>`, and a `lut` step that makes the plugin
  open a file itself — the first example where a *plugin* needs the filesystem
  rather than a language runtime. Rust, C++, JavaScript and Python implement it
  and agree byte for byte; `crates/host/tests/asset_e2e.rs` runs the same cases
  against every guest present and is where that claim is actually enforced.
- **A C++ guest**, via wasi-sdk. The project's claim is that C++ applications
  have no component-model plugin option; a C++ *host* only showed half of it.
- **`examples/host-cpp-asset`**, which decodes PNG with `stb_image`, routes a
  pipeline on `describe`, and writes the result back. `stb` is fetched with a
  pinned commit like GoogleTest rather than vendored.
- **`limits.transfer`** (ADR-0010's neighbour, found by the PNG host). Wasmtime
  meters what the *host* allocates lifting a guest's return value, separately
  from `limits.fuel`, and watoots did not expose it — so a plugin could not
  return more than about 2.79 MB and the error named a budget the manifest had
  no word for. Note the unit: on the dynamic path a `list<u8>` costs 48 per
  byte, because lifting produces a `Val` per element. `docs/MANIFEST.md` carries
  the arithmetic.
- **Reload** ([ADR-0010](docs/adr/0010-reload.md)). `Plugin::reload`,
  `wt_plugin_reload`, `wt::Plugin::Reload`, `watoots reload`. State crosses as
  typed WIT values through optional `save-state` / `restore-state` exports.
  Reload re-runs the import-intersection check — a plugin must not acquire a
  capability by being updated — and a refused reload leaves the old instance
  running, un-entered.
- **`PluginProfile::wave_nanos`**, a fourth bucket. The profiler measured only
  the call and not the WAVE text conversion around it, so it reported 52 ms of a
  177 ms call and charged the difference to marshalling. Marshalling is what the
  component model costs; this is what the *untyped* path costs, and only one of
  them would go away under `bindgen!`.
- `PluginStats::reloads`.

### Fixed

- A `limits.transfer` overrun reported `ErrorKind::Trap`, so a ceiling read as
  misbehaviour and sent whoever installed the plugin to debug the plugin.
- A leak in the C++ lint guest: an import's arguments belong to the caller and
  there is no post-return on the guest side. 128 KiB growing to 1.0 MiB over
  20 000 calls, now flat.
- A `Plugin` outliving its `Host` lost the epoch ticker, so `limits.timeout`
  silently stopped firing on an already-running plugin.

### Changed

- `examples/wit/` now holds one directory per world, because a WIT directory is
  a single package and two worlds cannot share one.
- Failure prose in the asset world is explicitly **not** conformance surface.
  Pinning it made the second guest reproduce five behaviours of Rust's standard
  library; a host branches on the case.

### Not built

- **Same-binary checkpoint/restore.** Tried, and it cannot be done: a
  component's memory, globals and tables are unreachable from wasmtime's public
  API. See [ADR-0010](docs/adr/0010-reload.md) for what would have to change.

## [0.2.0] — 2026-09-04

Everything here is additive: a 0.1.0 manifest and a 0.1.0 plugin still work.

### Added

- **`wasi:logging` as a granted capability** (ADR-0006). `logging = "warn"` in
  the manifest, absence denies, and the application supplies the sink. Adopting
  the proposal rather than inventing `watoots:log` means a plugin written for
  another host logs correctly here. `limits.log_bytes` and `limits.log_messages`
  cap volume per call, because fuel bounds iterations and not the bytes each one
  pushes into your log pipeline.
- **`watoots inspect` answers "what can this plugin do"** rather than listing
  imports: a granted filesystem names its directories, an interface *your
  application* must serve is separated from a permission you failed to grant,
  and a capability granted but never imported is reported — over-granting is
  invisible in an import list because the evidence is an import that is absent.
  `--imports` keeps the old per-import view.
- **`Host::check_targets` and `watoots inspect --targets`** (ADR-0007), closing
  the half the import-intersection check never covered: whether a plugin
  *exports* what you are about to call. Also `watoots wit semver-check`.
- **`Plugin::stats`** (ADR-0006): calls, fuel consumed, peak memory, log volume,
  imports declared and denied. Observed at the boundary, so a guest can neither
  forge nor inflate them — a refused memory growth does not raise the peak.
- **`Plugin::profile` and `watoots profile`** (ADR-0009): time split into guest,
  host call, and boundary marshalling, per WIT function, plus `--firefox` for a
  Firefox Profiler JSON. Off by default and refused alongside a trace hook,
  since a trace recorded under a profiler records a run nobody can reproduce.
- **Property tests** (ADR-0008), run by `cargo test`, whose oracle is that
  recording a session and replaying it reports faithful, plus `watoots fuzz` for
  longer campaigns against a real plugin.

### Fixed

Three of four defects the property tests found on their first run:

- The trace text encoding kept a manifest byte for byte. It used `lines()`,
  which cannot tell `"a\nb"` from `"a\nb\n"`, so a manifest recorded from a
  TOML file with no final newline gained one. The only one of the four
  reachable from a real recording.
- An `arg` or `value` line carries its value verbatim. An empty value used to
  render as a line the parser then rejected — a file the encoder produced and
  the decoder refused.
- Replay steps over crossings it does not serve. A plugin that both logged and
  called an application host function stalled the cursor on its own log events
  and reported the next real import as a divergence.

The fourth is upstream: `wasm-wave` collects flag labels into a `BTreeMap`, so a
flag set of two or more comes back alphabetically. Record and replay are
unaffected, since traces compare rendered text.

- The profiler no longer charges instantiation to the first call. Start
  functions and allocator setup run before any call exists, and counting them
  put `guest` at 365% of `wall` on a single-call run — which forced the
  marshalling remainder, the one number the profiler exists to report, to zero.

### Changed

- `wt_host_inspect` returns the capability summary; `wt_host_inspect_imports`
  returns the per-import list.
- clang-format and clang-tidy are pinned to LLVM 22 (ADR-0003). Ubuntu's archive
  ships 18, whose Google style disagrees with recent releases.
- `tools/tidy.sh` reconfigures its build tree every run. It used to reuse a
  stale compile database, which silently dropped translation units — it checked
  four while CI checked six, and reported clean while CI failed.

## [0.1.0] — 2026-09-03

First release. Both halves of the project work end to end.

### Host library (`watoots`)

- Manifest-driven permissions: `fs.read` / `fs.write`, `net`, `env`, `clocks`,
  `random`, with everything denied unless granted.
- **Import-intersection check at load time.** A component's declared imports are
  intersected with the manifest before instantiation, so an ungranted capability
  is a load error rather than a runtime trap. `Host::inspect` answers the same
  question without instantiating anything.
- Per-call, per-plugin limits: memory, fuel, and an epoch deadline, re-armed
  before every call.
- Dynamic calls via `Val` and WAVE text, alongside the typed path.
- Serving the application's own WIT interfaces to plugins.
- Precompile cache keyed on the engine's compatibility hash plus the component
  bytes, so reusing a `.cwasm` is sound and not merely fast.
- A registry: many plugins, one engine, a store each.
- Determinism knobs on by default: NaN canonicalisation, deterministic
  relaxed-SIMD, a pinned wall clock, a stepping monotonic clock, and seeded
  random generators.
- A trace hook over every host/plugin crossing.

### C API (`watoots-capi`)

- `wt_*` C surface generated by cbindgen and committed, so consumers need
  neither cargo nor cbindgen.
- `watoots.hpp`: a C++20 RAII wrapper. `wt::Result<T>` is `std::expected` where
  the standard library has it and a shim otherwise.
- An installable CMake package; `find_package(watoots)` and link
  `watoots::capi`.
- Panics are caught at every entry point and reported as `WT_ERR_INTERNAL`.

### Record and replay (`watoots-trace`)

- A WIT-level trace format in two encodings, text and binary, losslessly
  interconvertible. Values are WAVE, so a trace diffs cleanly and can be edited
  by hand.
- A recorder that fails at `finish` rather than returning a trace with holes.
- A replay runner that answers a plugin's imports from the recording and reports
  the first divergence. The manifest travels in the trace header, so replay
  needs only the trace and the component.

### CLI (`watoots`)

- `inspect`, `run`, `record`, `replay`, `trace fmt`.
- `replay --assert` exits non-zero on divergence; `--emit-test` writes a Rust
  test that performs the replay.

### Examples

- One WIT world implemented in Rust, JavaScript and Python, three policies, and
  a C++ host application that runs all three.

### Known limitations

- A non-empty `permissions.net` allowlist is refused rather than over-granted;
  `net = []` grants the socket interfaces with nothing reachable.
- Resource handles have no WAVE spelling, so worlds passing resources across the
  boundary cannot be traced.
- No host-to-guest reentrancy, and no async or stream imports.

See [docs/SECURITY.md](docs/SECURITY.md) for what the sandbox does and does not
protect against.

[Unreleased]: https://github.com/vchance/watoots/compare/v0.5.0...HEAD
[0.5.0]: https://github.com/vchance/watoots/releases/tag/v0.5.0
[0.4.0]: https://github.com/vchance/watoots/releases/tag/v0.4.0
[0.3.0]: https://github.com/vchance/watoots/releases/tag/v0.3.0
[0.2.0]: https://github.com/vchance/watoots/releases/tag/v0.2.0
[0.1.0]: https://github.com/vchance/watoots/releases/tag/v0.1.0
