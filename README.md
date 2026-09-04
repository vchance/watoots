# watoots

> **Work in progress — v0.1.0 is tagged, not published.** crates.io holds
> only the `0.0.0` placeholders that reserve the names; 0.1.0 is not there, so
> build from the tag. The API can still change between 0.x releases. What is
> here works and is tested in CI: the host, the C API, record/replay, and three
> sample plugins in three languages over one WIT world. What is missing is
> anyone else's opinion. Read it, build it, tell me where it is wrong.

A batteries-included plugin host for native applications on the WebAssembly
component model, plus WIT-level record/replay. Built on Wasmtime 48 (LTS).

You want third parties to extend your app. You do not want their code to have
your process's privileges. watoots turns "I have a Wasmtime dependency" into "I
have a plugin system" — with a C API from day one, so C++ applications are
first-class rather than an afterthought.

## The manifest

This is the product. Everything else exists to enforce it.

```toml
[permissions]
fs.read  = ["${plugin_dir}", "${workspace}/**/*.md"]
fs.write = ["${plugin_dir}/cache"]
clocks   = "monotonic"          # durations, but no idea what day it is
random   = true
                                # `net` absent: no sockets, no HTTP

[limits]
memory  = "64MiB"
fuel    = 50_000_000            # per call
timeout = "200ms"               # per call
```

Everything is denied unless granted. There is no "allow all, then subtract".

The part that is not just a config file: **a component declares its imports in
the binary**, so they can be read without running it. At load time watoots
intersects what a plugin asks for against what you granted, and refuses anything
uncovered:

```console
$ watoots inspect lint.wasm -m policy.toml
  ok   wasi:clocks/monotonic-clock@0.2.9  [permissions.clocks = "monotonic"]
  DENY wasi:sockets/tcp@0.2.9             [permissions.net]

1 import(s) are not granted
```

That is a **load** error, not a runtime trap. No guest code has run. You learn a
plugin wants the network when you install it, not at 3am when it first reaches
for a socket — and the exit code is non-zero, so it works as a CI gate.

Full reference: **[docs/MANIFEST.md](docs/MANIFEST.md)**.
Limits of the sandbox, stated plainly: **[docs/SECURITY.md](docs/SECURITY.md)**.

## Record and replay

Every call between an application and a plugin already goes through the host
library, so recording is a hook rather than an instrumentation pass:

```console
$ watoots record lint.wasm -m policy.toml -c lint -o bug.wave -- '"notes.md"' '"TODO\n"'
wrote bug.wave (6 crossings)
```

The trace is text, and readable:

```
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
of your application:

```console
$ watoots replay bug.wave -c lint.wasm --assert
replay matched the trace (6 crossings)
```

Change one line of that file and CI notices:

```
replay diverged after 3 matching crossing(s):
event 3:
  expected: watoots:example/log@0.1.0#emit(hint, "9 diagnostic(s)")
  actual:   watoots:example/log@0.1.0#emit(hint, "2 diagnostic(s)")
```

`--emit-test` writes a Rust test that performs the replay, so a bug report
becomes a regression test with no host code around it.

This is deliberately *not* Wasmtime's own `rr`, which records at the
canonical-ABI level for bit-exact engine determinism in a binary format that is
explicitly not meant to be read. This is at the WIT level: diffable in review,
editable by hand. The two compose.

See it all in 90 seconds: `tools/demo.sh`.

## Any guest language, one host

`examples/` has the same linter in Rust, JavaScript and Python against one WIT
world, driven by one C++ host binary that is not recompiled between them.

The interesting part is that their policies differ, and none of the plugins
*uses* what it is granted — the import list reflects the toolchain, not the
author:

| | Rust | JavaScript | Python |
|---|:-:|:-:|:-:|
| monotonic clock, environment | ✓ | ✓ | ✓ |
| wall clock, filesystem | | ✓ | ✓ |
| random, socket interfaces | | | ✓ |

`std` pulls in the clock. StarlingMonkey needs the wall clock for `Date`.
CPython links sockets at startup. You can see the whole bill before running
anything.

## Rust

```rust
use watoots::Host;

let host = Host::builder().manifest_from_file("policy.toml")?.build()?;
let mut plugin = host.load("lint.wasm")?;
let out = plugin.call_wave("lint", &[r#""notes.md""#, r#""TODO\n""#])?;
```

## C and C++

The C API is a v0.1 deliverable, not a follow-on. `watoots.hpp` is a RAII
wrapper over it; a consumer links a prebuilt library and reads committed
headers, and never needs a Rust toolchain.

```cpp
wt::HostBuilder builder;
builder.ManifestFromFile("policy.toml");
auto host = builder.Build();
auto plugin = host->Load("lint.wasm");
auto out = plugin->Call("lint", args);
```

```cmake
find_package(watoots REQUIRED)
target_link_libraries(my_app PRIVATE watoots::capi)
```

## Building

```sh
cargo test                              # host, trace, CLI
cargo clippy --all-targets -- -D warnings

tools/build-plugins.sh                  # sample plugins (Rust, JS, Python)

cmake --preset dev && cmake --build --preset dev && ctest --preset dev
tools/format.sh --check                 # clang-format, Google style
tools/tidy.sh                           # clang-tidy
```

Rust 1.95+ (whatever Wasmtime 48 requires), CMake 3.28+, a C++20 compiler.

## Status

v0.1. Both halves work and are tested end to end. Not yet published anywhere;
see [docs/SPEC.md](docs/SPEC.md) for what is deliberately *not* built, and
[docs/adr/](docs/adr/) for the decisions and why.

Licensed under Apache-2.0 WITH LLVM-exception.
