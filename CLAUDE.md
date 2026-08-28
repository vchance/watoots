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
- Guest target: **WASI 0.2.x** for v0.1. Keep the permission model 0.3-shaped
  (per-interface grants; no `wasi:io` assumptions) but do not depend on
  `wasmtime-wasi::p3` — it is documented as not production-ready.
- **C API is a v0.1 deliverable**, not a follow-on. Every public Rust API
  should be designed with a C surface in mind (opaque handles, no generics
  across the boundary, error codes + message strings).
- Default-deny everything: no network, no filesystem, no wall clock unless
  the manifest grants it.
- We complement Wasmtime's engine-level `rr` feature; we never record guest
  memory or engine state. We do not reimplement `wasm-tools component
  semver-check` or `targets` — call/wrap them.

## Layout (workspace exists; crates are v0.0.0 placeholders until M1 fills them)
- `crates/host/`        core library (`Host`, `Plugin`, manifest, limits, registry, cache)
- `crates/host-capi/`   cbindgen C API + `include/` C++ RAII header + CMake package
- `crates/trace/`       trace format (WAVE text + binary), recorder shim, replay runner
- `crates/cli/`         `watoots` binary: `inspect`, `run`, `record`, `replay`, `trace fmt`
- `examples/wit/`       the sample plugin world used by all examples and tests
- `examples/plugins/`   Rust, JS (ComponentizeJS), Python (componentize-py) sample plugins
- `examples/host-cpp/`  minimal C++ host app proving the C API
- `docs/adr/`           architecture decision records (one file per decision)

## Naming (ADR-0001)
- Project/crate/CLI: `watoots`. Crates: `watoots` (host), `watoots-capi`, `watoots-trace`, `watoots-cli`.
- C prefix: `wt_` (types `wt_host_t`, functions `wt_host_new`). Header `watoots.h`, C++ `watoots.hpp`.
- Env vars: `WATOOTS_*`.

## Conventions
- Rust 2024 edition, MSRV = whatever Wasmtime 48 requires.
- `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` must pass.
- Any decision listed under "Open decisions" in the spec gets an ADR in
  `docs/adr/NNNN-title.md` when made. Don't silently pick. ADR-0001 (name) and ADR-0002 (license: Apache-2.0 WITH LLVM-exception) are done.
- Prefer `wasmtime::component::Val` + WAVE for dynamic calls; `bindgen!` only
  where the Rust host has a static world.
- Tests live next to code; integration tests under `crates/*/tests/` use the
  sample plugins in `examples/`.

## Environment notes (as of 2026-08-28)
- Rust 1.97.1, cargo 1.97.1, CMake 4.4.3 present.
- `wasm32-wasip2` target is **not installed**: `rustup target add wasm32-wasip2`.
- `wasm-tools`, `wac`, `wit-bindgen-cli` are not confirmed installed; check with
  `which` before relying on them (`cargo install wasm-tools wac-cli wit-bindgen-cli`).

## Milestones (from the spec)
M1 spike → M2 host core → M3 C API + polyglot proof (first publishable) →
M4 record/replay → M5 ship v0.1 → M6 v0.2 from feedback.
Current: **M1**.
