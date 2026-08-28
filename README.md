# watoots

A batteries-included plugin host for native applications on the WebAssembly
component model, plus a WIT-level record/replay tool. Built on Wasmtime 48 (LTS).

**Status:** M3 complete — the first publishable state. A C API, a C++ RAII
header, an installable CMake package, and a C++ host application running the
same WIT world implemented in Rust, JavaScript and Python. Record/replay (M4) is
next. Start with [`docs/SPEC.md`](docs/SPEC.md).

```sh
tools/build-plugins.sh    # sample plugins (Rust, JS, Python)
cargo test                # Rust: host library (builds the sample plugin)
cargo run -p watoots --example minimal
cargo clippy --all-targets -- -D warnings

cmake --preset dev && cmake --build --preset dev && ctest --preset dev
tools/format.sh --check   # clang-format, Google style
tools/tidy.sh             # clang-tidy
```

Name locked 2026-08-28 — see `docs/adr/0001-name.md`.
