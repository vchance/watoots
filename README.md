# watoots

A batteries-included plugin host for native applications on the WebAssembly
component model, plus a WIT-level record/replay tool. Built on Wasmtime 48 (LTS).

**Status:** M2 complete. `crates/host` loads components, refuses imports the
manifest does not grant, enforces per-call fuel/deadline/memory limits, serves
the application's own interfaces, caches compiled code, and observes every
host/plugin crossing through a trace hook. A Rust sample plugin runs end to end
in `examples/plugins/rust-lint`. The C API, trace, and CLI crates are still
placeholders. Start with [`docs/SPEC.md`](docs/SPEC.md).

```sh
cargo test                # Rust: host library (builds the sample plugin)
cargo run -p watoots --example minimal
cargo clippy --all-targets -- -D warnings

cmake --preset dev && cmake --build --preset dev && ctest --preset dev
tools/format.sh --check   # clang-format, Google style
tools/tidy.sh             # clang-tidy
```

Name locked 2026-08-28 — see `docs/adr/0001-name.md`.
