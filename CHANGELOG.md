# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.3.0] — 2026-09-06

**One breaking manifest change:** `permissions.net` no longer takes a list of
hosts, and a manifest using `net = []` will not parse. See *Removed* below;
`net = "linked"` is the replacement, and the parse error says so.

**One C ABI change:** `wt_plugin_stats_t` gained a trailing `reloads` field, so
a caller compiled against a 0.2.0 header reads a struct one field short. Nothing
is published to crates.io, so nobody is holding a stale header — but it is an
ABI change and this is where it is said.

Everything else is additive for manifests and plugins.

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

- **A written WASI p3 position, replacing an open question in the spec.**
  [ADR-0013](docs/adr/0013-wasi-p3-position.md): the host stays on 0.2, and the
  two conditions for adopting p3 are written down and checkable. The
  disqualifying one is that `wasmtime-wasi`'s p3 module states security fixes
  limited to wasip3 get no patch release. Two claims in `docs/SPEC.md` were
  re-checked and corrected — `wasm32-wasip3` is still Tier 3 in the rustc book
  (the promotion proposal was accepted, which is not the same), and it is Spin
  4.1.0 rather than 4.0 that ships p3 on Wasmtime 48.
- "The permission model is 0.3-shaped" is now a test rather than an intention:
  `crates/host/tests/wasip3_shape.rs` classifies the p3 interface set read from
  `wasmtime-wasi` 48.0.1's own WIT.
- `examples/wit/` now holds one directory per world, because a WIT directory is
  a single package and two worlds cannot share one.
- Failure prose in the asset world is explicitly **not** conformance surface.
  Pinning it made the second guest reproduce five behaviours of Rust's standard
  library; a host branches on the case.

### Fixed (security)

- **0.3's wall clock would have been classified as the monotonic one.** WASI 0.3
  renames `wasi:clocks/wall-clock` to `system-clock` and adds `timezone`; the
  capability table matched `wall-clock` by name and fell through on
  `("clocks", _)` to `MonotonicClock`. A manifest saying `clocks = "monotonic"`
  — the setting chosen precisely to keep real time away from a plugin — would
  have admitted the real-time clock. `clocks` is the only WASI package split
  across two different capabilities, which is why a fallthrough there
  over-grants where the `filesystem` and `sockets` ones do not. The arm is now
  exhaustive and an unread `clocks` interface denies.

  Latent rather than exploitable: the host links wasip2 only, so a p3 component
  fails at instantiation either way. But classification runs first, so
  `watoots inspect` would have reported a p3 plugin as satisfying a monotonic-only
  policy. Found by writing [ADR-0013](docs/adr/0013-wasi-p3-position.md), and
  pinned by `crates/host/tests/wasip3_shape.rs`.

### Removed

- **The `permissions.net` host allowlist, which never worked and could not.**
  `net` is now `"deny"` (the default) or `"linked"`, and never a list. A
  non-empty list was already refused at host-build time as "not enforced yet";
  the promise cannot be kept at this layer, because `wasmtime-wasi`'s
  `socket_addr_check` is handed a resolved `SocketAddr` and never the hostname
  that produced it. A manifest key that reads as a restriction and enforces
  nothing is worse than no key — it misleads the person reviewing a policy
  before installing a plugin, which is what the manifest is *for*. Hostname
  policy belongs to whatever the application serves behind `wasi:http`, where
  the name still exists. [ADR-0012](docs/adr/0012-no-net-allowlist.md).

  **Migrating:** `net = []` becomes `net = "linked"`; drop any named hosts and
  apply the rule in your own `wasi:http` implementation. `net = ["example.com"]`
  never granted `example.com` in any released version. `Permissions::net` is now
  a `NetGrant` rather than an `Option<Vec<String>>`, and `is_granted()` replaces
  `is_some()`.

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

[0.3.0]: https://github.com/vchance/watoots/releases/tag/v0.3.0
[0.2.0]: https://github.com/vchance/watoots/releases/tag/v0.2.0
[0.1.0]: https://github.com/vchance/watoots/releases/tag/v0.1.0
