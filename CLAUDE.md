# watoots — project context

## What this is
A Rust crate + C API that turns "I have a Wasmtime dependency" into "I have a
sandboxed plugin system" for native apps, using the WebAssembly component model.
Second piece: WIT-level record/replay of host↔component crossings, so a plugin
bug can be replayed (and kept as a regression test) without the host app.

Read `docs/SPEC.md` before doing anything. It is the source of truth for scope,
milestones, and what we deliberately do NOT build. `docs/scoping.html` is the
same content as the published scoping page (read-only reference).

## Hard constraints
- Engine: **Wasmtime 48.x** (LTS line). Do not bump majors without an ADR.
- Guest target: **WASI 0.2.x**. Do not depend on `wasmtime-wasi::p3`: it
  documents that security fixes limited to wasip3 get no patch release, which a
  sandbox cannot take. ADR-0013 records the two conditions that would change
  this, so check it rather than re-arguing. The permission model stays
  0.3-shaped (per-interface grants; no `wasi:io` assumptions) and
  `crates/host/tests/wasip3_shape.rs` enforces that against wasmtime-wasi 48's
  own p3 WIT — note that `clocks` is the one package split across two
  capabilities, so its match arm is exhaustive on purpose.
- **C API is a v0.1 deliverable**, not a follow-on. Every public Rust API
  should be designed with a C surface in mind (opaque handles, no generics
  across the boundary, error codes + message strings).
- Default-deny everything: no network, no filesystem, no wall clock unless
  the manifest grants it. **The single exception is `[signature]`**, which is
  off when absent so that upgrading does not break existing plugins (ADR-0014);
  everywhere else, absence denies.
- We complement Wasmtime's `rr` feature, which records at the *same* level we
  do — wasmtime#11284's goal is "to purely capture guest-host boundary
  crossings". What it names as a non-goal is a human-readable trace format,
  "an independent tool over the low-level trace". That is us. We never record
  guest memory or engine state. We do not reimplement `wasm-tools component
  semver-check` or `targets` — call/wrap them.

## Layout
- `crates/host/`        core library (`Host`, `Plugin`, manifest, limits, registry, cache)
- `crates/host-capi/`   cbindgen C API + `include/` C++ RAII header + CMake package
- `crates/trace/`       trace format (WAVE text + binary), recorder shim, replay runner
- `crates/cli/`         `watoots` binary: `inspect`, `run`, `record`, `replay`, `trace fmt`
- `examples/wit/lint/`  the lint world (four guests)
- `examples/wit/asset/` the asset-pipeline world
- `examples/plugins/`   eight guests: Rust, C++ (wasi-sdk), JS (ComponentizeJS),
                        Python (componentize-py), each implementing both worlds
- `examples/host-cpp/`  C++ host for the lint world, proving the C API
- `examples/host-cpp-asset/` C++ host for the asset world: decodes PNG with stb,
                        routes a pipeline on `describe`, writes PNG back
- `docs/adr/`           architecture decision records (one file per decision)

## Naming (ADR-0001)
- Project/crate/CLI: `watoots`. Crates: `watoots` (host), `watoots-capi`, `watoots-trace`, `watoots-cli`.
- C prefix: `wt_` (types `wt_host_t`, functions `wt_host_new`). Header `watoots.h`, C++ `watoots.hpp`.
- Env vars: `WATOOTS_*`.

## Conventions
- Rust 2024 edition, MSRV = whatever Wasmtime 48 requires.
- C/C++ (ADR-0003): Google style, C++20 floor for shipped headers, no
  exceptions. `cmake --preset dev && ctest --preset dev`; `tools/format.sh
  --check` and `tools/tidy.sh` must pass. clang-tidy needs `brew install llvm`.
- `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` must pass.
- Any decision listed under "Open decisions" in the spec gets an ADR in
  `docs/adr/NNNN-title.md` when made. Don't silently pick. ADR-0001 (name), ADR-0002 (license: Apache-2.0 WITH LLVM-exception), and
  ADR-0003 (C++ toolchain), ADR-0004 (WAVE), ADR-0005 (cargo from
  CMake), ADR-0006 (`wasi:logging` yes, guest-emitted metrics no) and
  ADR-0007 (link `wit-component`, don't shell out), ADR-0008 (proptest on
  stable; record/replay is the fuzzing oracle), ADR-0009 (profile at the
  boundary; the timeout outranks the sampler) and ADR-0010 (reload carries
  state as WIT values; checkpoint is not buildable), ADR-0011 (an audit trail is
  a third hook) and ADR-0012 (no `permissions.net` host allowlist; `net` is
  `"deny"` or `"linked"`) ADR-0013 (stay on WASI 0.2; conditions for p3) and
  ADR-0014 (pinned-key signature verification at load; no Sigstore keyless)
  are done.
- Prefer `wasmtime::component::Val` + WAVE for dynamic calls; `bindgen!` only
  where the Rust host has a static world.
- Tests live next to code; integration tests under `crates/*/tests/` use the
  sample plugins in `examples/`.

## Environment notes (as of 2026-09-06)
- Rust 1.97.1, cargo 1.97.1, CMake 4.4.3 present. MSRV is 1.95 (Wasmtime 48).
- `wasm32-wasip2` target **is installed**.
- `wasm-tools`, `wac` and `cargo-component` are **not** installed. Host tests
  build their components from WAT inline, which keeps them hermetic.
- **`wit-bindgen-cli` 0.61.1 and wasi-sdk 34 are installed** for the C++ guest,
  the latter at `~/.local/share/wasi-sdk-34.0-arm64-macos`. `wasm32-wasip2-clang++`
  emits a component directly — `wasm-component-ld` ships with the SDK, so there
  is no `wasm-tools component new` step. The generated bindings are C and must
  be compiled with `clang`, not `clang++`, or the component-type force-link
  symbol is mangled and the link fails on a name nothing explains.
- **LLVM 22 is the pinned major for clang-format and clang-tidy** (ADR-0003).
  `brew install llvm@22`; `tools/format.sh` and `tools/tidy.sh` find it without
  configuration, and `format.sh` warns if it falls back to another version.
  Ubuntu's archive ships 18, whose Google style disagrees — that is why CI
  installs from `apt.llvm.org` rather than `apt-get install clang-format`.

## Milestones (from the spec)
M1 spike → M2 host core → M3 C API + polyglot proof (first publishable) →
M4 record/replay → M5 ship v0.1 → M6 v0.2 from feedback.
Current: **M6**. M1-M5 are done and v0.2.0 is tagged. `crates/host` has the
engine, manifest, import-intersection check, per-call limits, registry,
precompile cache, dynamic `Val`/WAVE calls, determinism knobs and the trace
hook; `crates/host-capi` has the cbindgen C API, the C++ RAII header and an
installable CMake package; `crates/trace` has the trace format (text + binary),
recorder and replay runner; `crates/cli` has `watoots inspect|diff|run|record|replay|
trace fmt`. `examples/` has two WIT worlds: `lint` (small, hermetic, the smoke test) and
`asset` (an image pipeline with variants, a result, large `list<u8>` payloads
and a filesystem capability). Four guests implement each, eight policies derived
from what each toolchain actually imports, and two C++ hosts.

M6 so far: `inspect` rewritten as a capability summary plus `--targets` and
`wit semver-check`; property tests whose oracle is record/replay; `PluginStats`;
a boundary profiler; reload; an audit trail (ADR-0011); a compiled-component
cache; signature verification at load (ADR-0014); and `watoots diff`.
**Every v0.2 candidate in the spec is done.**

Decided since: no `permissions.net` allowlist (ADR-0012, breaking) and stay on
WASI 0.2 with written conditions for p3 (ADR-0013).

M5 shipped: `docs/MANIFEST.md`, a root `SECURITY.md` (reporting policy)
alongside `docs/SECURITY.md` (threat model), `CONTRIBUTING.md`,
`CODE_OF_CONDUCT.md`, issue forms, a manifest-first README, `tools/demo.sh`,
`CHANGELOG.md`. The repo is public at `github.com/vchance/watoots` and crate
metadata points at it. Tags `v0.0.0`, `v0.1.0`, `v0.2.0` and `v0.3.0`
are all on the remote; **crates.io has only the `0.0.0` name placeholders** and
no release is published there, so the README tells people to build from the tag.

**`main` is ahead of `v0.3.0`.** Work landing after a tag goes in the
CHANGELOG's `[Unreleased]` section, not the tagged one -- appending to a
released version's entry claims things that version does not contain, which is
how a breaking `permissions.net` change briefly came to be filed under an
already-shipped release. The next release is **0.4.0**, not a patch, because of
that change. `Cargo.toml`'s version is bumped at release time, not before.

**Announcing is Von's call and is not to be raised.** It is the last M5 item on
paper; do not offer it, recommend it, or list it as outstanding.

Four things the real guests taught us, worth knowing before debugging a denial:
a `wasm32-wasip2` Rust guest imports `wasi:clocks/monotonic-clock` and
`wasi:cli/environment` via `std` whether or not the author uses them; a WIT
interface imported only for its types has no callable functions and is not a
capability (`Requirement::TypesOnly`); CPython links `wasi:sockets`
unconditionally, which is why `net` grants the *import* separately from any
reachability — `net = "deny"` (the default) refuses the import, `net = "linked"`
allows it with nothing reachable, and there is no host allowlist because
nothing at this layer could enforce one (ADR-0012); and replay must serve
every import a component *declares*, not just the ones a recording exercised.
