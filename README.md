# watoots

A batteries-included plugin host for native applications on the WebAssembly
component model, plus a WIT-level record/replay tool. Built on Wasmtime 48 (LTS).

**Status:** M1 complete. `crates/host` loads a component, refuses imports the
manifest does not grant, and enforces per-call fuel, deadline, and memory
limits. The C API, trace, and CLI crates are still placeholders. Start with
[`docs/SPEC.md`](docs/SPEC.md).

```sh
cargo test                # Rust: host library
cargo clippy --all-targets -- -D warnings

cmake --preset dev && cmake --build --preset dev && ctest --preset dev
tools/format.sh --check   # clang-format, Google style
tools/tidy.sh             # clang-tidy
```

Name locked 2026-08-28 — see `docs/adr/0001-name.md`.
