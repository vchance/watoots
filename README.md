# watoots

A batteries-included plugin host for native applications on the WebAssembly
component model, plus a WIT-level record/replay tool. Built on Wasmtime 48 (LTS).

**Status:** M1. The Rust crates are still empty placeholders; the C/C++
toolchain is set up and its tests pass. Start with [`docs/SPEC.md`](docs/SPEC.md).

```sh
cmake --preset dev && cmake --build --preset dev && ctest --preset dev
tools/format.sh --check   # clang-format, Google style
tools/tidy.sh             # clang-tidy
```

Name locked 2026-08-28 — see `docs/adr/0001-name.md`.
