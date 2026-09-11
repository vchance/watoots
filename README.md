# watoots

> **Work in progress — v0.5.0 is tagged, not published.** crates.io holds
> only the `0.0.0` placeholders that reserve the names, so build from the tag.
> The API can still change between 0.x releases. What is here works and is
> tested in CI: the host, the C API, record/replay, `wasi:logging`, a boundary
> profiler, property tests whose oracle is replay itself, and three sample
> plugins in three languages over one WIT world. Read it, build it, tell me
> where it is wrong.

A batteries-included plugin host for native applications on the WebAssembly
component model, plus WIT-level record/replay. Built on Wasmtime 48 (LTS).

You want third parties to extend your app. You do not want their code to have
your process's privileges. watoots turns "I have a Wasmtime dependency" into "I
have a plugin system" — with a C API from day one, so C++ applications are
first-class rather than an afterthought.

## The manifest

The front door, and the thing worth looking at first.

```toml
[permissions]
fs.read  = ["${plugin_dir}", "${workspace}/**/*.md"]
fs.write = ["${plugin_dir}/cache"]
clocks   = "monotonic"          # durations, but no idea what day it is
random   = true
logging  = "warn"               # wasi:logging, warn and above
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
$ watoots inspect py_lint.wasm -m policy.toml
capabilities
  filesystem   DENY   wanted; no filesystem granted
  network      DENY   wanted; no sockets, no HTTP
  clock        ok     monotonic only - durations, not dates
  environment  ok     may read an empty environment
  random       DENY   wanted; no random granted
  logging      -      not requested, not granted

publisher
  signature    WARN   not verified - any bytes at this path load, with everything granted above

your application must serve
  watoots:example/log@0.1.0

27 import(s): 14 need no grant, 10 not granted
```

It answers "what can this plugin do", not "what does it import" — `--imports`
gives you the raw list. Three distinctions it makes that a list cannot: a
granted filesystem names the directories; an interface *your application* is
expected to serve is separated from a permission you failed to grant; and a
capability granted but never imported is reported, because over-granting shows
up as an import that is *absent*.

That is a **load** error, not a runtime trap. No guest code has run. You learn a
plugin wants the network when you install it, not at 3am when it first reaches
for a socket — and the exit code is non-zero, so it works as a CI gate.

Full reference: **[docs/MANIFEST.md](docs/MANIFEST.md)**.
Limits of the sandbox, stated plainly: **[docs/SECURITY.md](docs/SECURITY.md)**.

## Who wrote it

The manifest says what a plugin may *do*. It cannot say the plugin is the one
you think it is — swap the file on disk and the replacement inherits every
grant you gave it. That is the gap signatures close, and it is the only part of
watoots that can.

```toml
[signature]
keys = ["""
-----BEGIN PUBLIC KEY-----
MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEzuhbtxgdBr7pXOlkrACLK8+PXkOL
WmxJSG+8X0cSE6TaYhzhil1GgjEIbsI9QoDGb1DR6/EBuxvg2E4ejBuqDg==
-----END PUBLIC KEY-----
"""]
```

The format is what `cosign sign-blob` already writes, so nothing new has to be
invented or installed:

```sh
cosign sign-blob --key cosign.key --output-signature lint.wasm.sig lint.wasm
```

`watoots run lint.wasm` reads `lint.wasm.sig` from beside it. A plugin that is
unsigned, signed by a key you did not list, or changed by one byte does not
load, is not compiled, and is not cached. **Reload re-verifies** — the moment
the code changes is the moment publisher identity matters most, and a check
your application does before calling `load` is one that reload skips.

A policy file has to say which it is: list `keys`, or say `required = false`.
There is no default, because "nobody thought about signing" and "we decided not
to" must not look the same in a file someone reviews. Running unsigned warns on
stderr every time and records a `loaded-unverified` audit event.

Deliberately narrow: pinned keys, offline, no network at load. No Sigstore
keyless identity, no certificate chains, no transparency log — those need a
maintained trust root inside `load`, which a sandbox library should not have.
Verify a bundle where you *fetch* the plugin, then hand watoots bytes and a key.
[ADR-0014](docs/adr/0014-signature-verification.md) has the argument.

## What it was allowed to do, afterwards

`AuditHook` records authorisation decisions — a plugin loaded or refused, each
import's verdict, a reload, a log line dropped by the level ceiling, a `[limits]`
ceiling spent, a load nobody verified. Never argument values, so an audit line
is safe to keep when a trace is not.

Off unless you install it; a library that writes to stderr uninvited is badly
behaved. `watoots run --audit` turns it on for the command line.

## Record and replay

**The part nobody else builds.** Deny-by-default is table stakes now — Spin,
wasmCloud and Wassette all lead with it — but a readable, host-free replay that
becomes a regression test does not exist anywhere else. Wasmtime has its own
record/replay work at the *same* level as this, and
[wasmtime#11284](https://github.com/bytecodealliance/wasmtime/pull/11284) lists
as an explicit non-goal:

> A human readable trace format. This belongs better in something like
> wit-bindgen, and/or as an independent tool over the low-level trace.

That is this. Every call between an application and a plugin already goes
through the host library, so recording is a hook rather than an instrumentation
pass:

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

## Before you ship an update

A plugin update that quietly wants more than the last one is the failure this
project exists to catch. `Plugin::reload` re-runs the whole grant check and
refuses a replacement that asks for more — correct, and also the worst moment to
find out. `watoots diff` is that refusal, previewed:

```console
$ watoots diff deployed.wasm candidate.wasm -m policy.toml
imports
  + wasi:filesystem/types@0.2.9                      NOT GRANTED -> permissions.fs.read / permissions.fs.write
  - wasi:random/random@0.2.9                         no longer needed

exports
  - lint                                             callers of this break

1 new import(s) the manifest does not grant; reload would refuse this build
1 export(s) removed; a host calling them breaks
```

Non-zero exit, so it works as a gate. The two problems are counted separately
because they are fixed in different places: a new capability is a line in your
manifest, a removed export is a change to the plugin or its callers.

This is not `wasm-tools component semver-check`, which compares two WIT
*packages* for structural compatibility and which watoots wraps rather than
reimplements as `watoots wit semver-check`. `diff` compares two compiled
*components* and answers the question about your policy.


See it all in 90 seconds: `tools/demo.sh`.

## Any guest language, one host

`examples/` has the same linter in Rust, C++, JavaScript and Python against one
WIT world, driven by one C++ host binary that is not recompiled between them.
C++ appears on both sides deliberately: the claim this project makes is that C++
applications have no component-model plugin option today, and a C++ *host* only
half demonstrates it.

The interesting part is that their policies differ, and none of the plugins
*uses* what it is granted — the import list reflects the toolchain, not the
author:

| | Rust | C++ | JavaScript | Python |
|---|:-:|:-:|:-:|:-:|
| monotonic clock, environment | ✓ | ✓ | ✓ | ✓ |
| wall clock | | ✓ | ✓ | ✓ |
| filesystem | | | ✓ | ✓ |
| random, socket interfaces | | | | ✓ |

`std` pulls in the clock. wasi-libc links a wall clock Rust's `std` does not.
StarlingMonkey needs it for `Date`. CPython links sockets at startup, which is
why `net` is a grant for the *import* and never for a reachable host. You can
see the whole bill before running anything.

A second world, `examples/wit/asset`, is an image pipeline with the same four
guests — larger payloads, a `variant`, a `result`, and the first example where
the *plugin* itself needs the filesystem rather than its language runtime.

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

tools/build-plugins.sh                  # sample plugins (Rust, C++, JS, Python)

cmake --preset dev && cmake --build --preset dev && ctest --preset dev
tools/format.sh --check                 # clang-format, Google style
tools/tidy.sh                           # clang-tidy
```

Rust 1.95+ (whatever Wasmtime 48 requires), CMake 3.28+, a C++20 compiler.

## Status

**v0.5.0**, pre-1.0: the API can still move between 0.x releases. Both halves
work and are tested end to end in CI. crates.io holds only the `0.0.0`
placeholders that reserve the names, so build from the tag.

See [docs/SPEC.md](docs/SPEC.md) for what is deliberately *not* built,
[CHANGELOG.md](CHANGELOG.md) for what changed and what breaks, and
[docs/adr/](docs/adr/) for the decisions and why.

Licensed under Apache-2.0 WITH LLVM-exception.
