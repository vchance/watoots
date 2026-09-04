# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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

[0.2.0]: https://github.com/vchance/watoots/releases/tag/v0.2.0
[0.1.0]: https://github.com/vchance/watoots/releases/tag/v0.1.0
