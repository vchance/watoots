# watoots

A batteries-included plugin host for native applications on the WebAssembly
component model, plus a WIT-level record/replay tool. Built on Wasmtime 48 (LTS).

**Status:** M4 complete. Both halves of the project work: a batteries-included
plugin host with a manifest-driven permission model and a C API, and WIT-level
record/replay that turns a plugin bug into a file, and that file into a
regression test. Start with [`docs/SPEC.md`](docs/SPEC.md).

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

## Record and replay

Every call between an application and a plugin already goes through the host
library, so recording one is a hook rather than an instrumentation pass:

```sh
watoots record lint.wasm -m policy.toml -c lint -o bug.wave -- '"notes.md"' '"TODO\n"'
```

The trace is text, and readable:

```
watoots-trace 1
component sha256:8239ab81...
plugin rust_lint
manifest
  [permissions]
  clocks = "monotonic"

export-call lint
  arg "notes.md"
  arg "TODO\n"
import-call watoots:example/log@0.1.0 emit
  arg hint
  arg "linting notes.md"
import-return watoots:example/log@0.1.0 emit
  unit
export-return lint
  value [{line: 1, column: 1, severity: error, message: "unresolved TODO"}]
```

Replaying needs the trace and the component, and nothing else — the manifest
travels in the header, and the recording answers the plugin's imports in place
of the application:

```sh
watoots replay bug.wave -c lint.wasm --assert   # exit 1 on any divergence
```

A divergence names the crossing where the two parted company:

```
replay diverged after 3 matching crossing(s):
event 3:
  expected: watoots:example/log@0.1.0#emit(hint, "9 diagnostic(s)")
  actual:   watoots:example/log@0.1.0#emit(hint, "2 diagnostic(s)")
```

`--emit-test` writes a Rust test that performs the replay, so a bug report
becomes a regression test with no host code around it.

This is deliberately not Wasmtime's own `rr`, which records at the
canonical-ABI level for bit-exact engine determinism in a binary format that is
explicitly not meant to be read. This is at the WIT level: diffable in review,
editable by hand. The two compose.
