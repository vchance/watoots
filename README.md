# watoots

> **v0.7.0 is tagged, not published.** crates.io holds only the `0.0.0`
> placeholders that reserve the names, so build from the tag. The API can still
> change between 0.x releases. Everything below works and is tested in CI.

Third-party code inside your native application, without your process's
privileges — and when that code goes wrong, **the bug is a file**: a readable
trace that replays without your application, and becomes a regression test.

The sandbox is the part everyone builds. The replay is the part nobody else
does. Built on Wasmtime 48 (LTS) and the WebAssembly component model, with a C
API from day one so C++ applications are first-class.

## Open a file

Every desktop application has this feature, and it is the most dangerous one
it has: a file the user got from the internet, parsed by code from a third
party, inside your process. Image, archive, document and font parsers are where
the memory-safety CVEs live.

`examples/host-cpp-preview` is a previewer whose format decoders are plugins.
The codec is on the untrusted side, under a policy that grants nothing the
codec asked for:

```toml
[permissions]
clocks = "monotonic"    # Rust's std links a clock whether or not you use it
env    = {}             # and the environment; the decoder touches neither

[limits]
memory  = "64MiB"
fuel    = 200_000_000
timeout = "2s"
```

Now open `examples/fixtures/preview/bomb.qoi`: a valid QOI file whose header
claims 16384×16384 — a 1 GiB image — with nothing else wrong. The decoder
cannot know your budget, so it asks for the memory.

```console
$ host_cpp_preview policy.toml bomb.qoi out.png rust_qoi.wasm
preview: bomb.qoi looks like qoi; decoding
preview: rust_qoi hit limits.memory
preview: qoi: decode: WT_ERR_LIMIT_EXCEEDED: rust_qoi: decode: limits.memory: the plugin asked for 1074921472 bytes, the manifest allows 67108864, and it could not continue without them
```

Exit code 1, one line naming the ceiling, process still running. With the codec
in-process that is an OOM kill — or, with a less honest header, the CVE.

That is a Tuesday. The user files "the viewer fails on my file". What happens
next is the reason this project exists.

## The bug, as a file

Every call between an application and a plugin already goes through the host
library, so recording is a hook, not an instrumentation pass. Record the call
the viewer made:

```console
$ watoots record rust_qoi.wasm -m policy.toml -c decode -o bug.wave -- '[113,111,105,102,...]'
wrote bug.wave (2 crossings)
watoots: rust_qoi: decode: limits.memory: the plugin asked for 1074921472 bytes, the manifest allows 67108864, and it could not continue without them
```

The call failed and the trace was written anyway — a failure is the recording
worth keeping. It is text. The manifest is in it, the offending bytes are in it,
the outcome is in it:

```
watoots-trace 1
component sha256:2e79da2ba0167987ccd1771a021dfcd578f0758796a4a4faa737b2a1a95d641d
plugin rust_qoi
manifest
  [permissions]
  clocks = "monotonic"
  ...
export-call decode
  arg [113, 111, 105, 102, 0, 0, 64, 0, 0, 0, 64, 0, 4, 0, ...]
export-return decode
  error WT_ERR_LIMIT_EXCEEDED "rust_qoi: decode: limits.memory: the plugin asked for 1074921472 bytes, ..."
```

Whoever gets the bug report reproduces it with the trace and the component, and
nothing else — no viewer, no policy file, no fixtures:

```console
$ watoots replay bug.wave -c rust_qoi.wasm --assert
replay matched the trace (2 crossings)
```

Now edit the file. Raise `memory` in the embedded manifest to `2GiB` and replay
again:

```
replay diverged after 1 matching crossing(s):
event 1:
  expected: decode to fail with WT_ERR_LIMIT_EXCEEDED
  actual:   decode returned Some("err(truncated(268434432))")
```

With the memory allowed, the decoder correctly reports that 1109 bytes cannot be
a 16384×16384 image. The bomb was two lies deep, and replay found the second
one by editing a text file. `--emit-test` writes a Rust test that performs the
replay, so the bug report becomes a regression test with no host code in it.

A plugin that *calls back into the application* has those crossings recorded
too, and answered on replay in place of your code. The lint world's plugins log
through a host function:

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

Wasmtime has its own record/replay work at the *same* level as this, and
[wasmtime#11284](https://github.com/bytecodealliance/wasmtime/pull/11284) lists
as an explicit non-goal:

> A human readable trace format. This belongs better in something like
> wit-bindgen, and/or as an independent tool over the low-level trace.

That is this. Wasmtime's `rr` records at the canonical-ABI level for bit-exact
engine determinism in a binary format that is not meant to be read; this is at
the WIT level, diffable in review and editable by hand. The two compose.

`tools/demo-preview.sh` runs the whole story above, signed, and ends on the
replay. **[docs/WRITING-A-PLUGIN.md](docs/WRITING-A-PLUGIN.md)** builds a
plugin from nothing in fifteen minutes.

## The manifest

What that policy file is, and why it is not just a config file. The decoder's
was short because a decoder needs nothing; here is the full vocabulary:

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
uncovered. A Python plugin under that policy, for instance — CPython links a
great deal the author never asked for:

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
gives you the raw list. That is a **load** error, not a runtime trap. No guest
code has run. You learn a plugin wants the network when you install it, not at
3am when it first reaches for a socket — and the exit code is non-zero, so it
works as a CI gate.

Full manifest reference: **[docs/MANIFEST.md](docs/MANIFEST.md)**.
What it costs, measured: **[docs/PERFORMANCE.md](docs/PERFORMANCE.md)**.
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
load, is not compiled, and is not cached. **Reload re-verifies.**

A policy file has to say which it is: list `keys`, or say `required = false`.
There is no default, because "nobody thought about signing" and "we decided not
to" must not look the same in a file someone reviews. Running unsigned warns on
stderr every time and records a `loaded-unverified` audit event.

Deliberately narrow: pinned keys, offline, no network at load. No Sigstore
keyless identity, no certificate chains, no transparency log — those need a
maintained trust root inside `load`, which a sandbox library should not have.
[ADR-0014](docs/adr/0014-signature-verification.md) has the argument.

## What it was allowed to do, afterwards

`AuditHook` records authorisation decisions — a plugin loaded or refused, each
import's verdict, a reload, a log line dropped by the level ceiling, a `[limits]`
ceiling spent, a load nobody verified. Never argument values, so an audit line
is safe to keep when a trace is not. Off unless you install it;
`watoots run --audit` turns it on for the command line.

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

Non-zero exit, so it works as a gate.

## Any guest language, one host

`examples/` has the same linter in Rust, C++, JavaScript and Python against one
WIT world, driven by one C++ host binary that is not recompiled between them.
Their policies differ, and none of the plugins *uses* what it is granted — the
import list reflects the toolchain, not the author:

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

Three worlds. `examples/wit/preview` is the previewer above — QOI in all four
languages and farbfeld in Rust, the codec on the untrusted side; every decoder
agrees with the reference to the byte and every one gets the bomb refused,
including the ones running inside SpiderMonkey and CPython. `examples/wit/asset`
is an image pipeline with a `variant`, a `result`, large payloads and a
filesystem capability. `examples/wit/lint` is the small hermetic world the test
suite is built on.

## Rust

```rust
use watoots::{Host, Val};

let host = Host::builder().manifest_from_file("policy.toml")?.build()?;
let mut plugin = host.load("rust_qoi.wasm")?;
let file = Val::List(bytes.iter().copied().map(Val::U8).collect());
let out = plugin.call("decode", &[file])?;          // typed
let fmt = plugin.call_wave("format", &[])?;         // or WAVE text: `"qoi"`
```

## C and C++

The C API is a v0.1 deliverable, not a follow-on. `watoots.hpp` is a RAII
wrapper over it; a consumer links a prebuilt library and reads committed
headers, and never needs a Rust toolchain. Values cross either as WAVE text or
as typed `wt::Val`s — a codec's argument is a file, and a file rendered as
`[113, 111, 105, ...]` is four characters per byte on both sides of the
boundary, so bytes go across as bytes:

```cpp
wt::HostBuilder builder;
builder.ManifestFromFile("policy.toml");
auto host = builder.Build();
auto plugin = host->Load("rust_qoi.wasm");

std::vector<wt::Val> args;
args.push_back(wt::Val::Bytes(file_bytes));
auto image = plugin->Call("decode", args);
if (auto pixels = (*image)->Payload()->Field("pixels")->AsBytes()) { ... }
```

```cmake
find_package(watoots REQUIRED)
target_link_libraries(my_app PRIVATE watoots::capi)
```

Both paths are one call underneath — same limits, same trace, same audit — so a
recording made through either replays through either, and a host passing a
`string` where the world says `u32` gets `WT_ERR_INVALID_ARGUMENT` naming the
argument, not a trap that poisons the plugin.

## Building

```sh
cargo test                              # host, trace, CLI
cargo clippy --all-targets -- -D warnings

tools/build-plugins.sh                  # sample plugins (Rust, C++, JS, Python)
tools/demo-preview.sh                   # the previewer, the bomb, and the replay
tools/demo.sh                           # the lint world: deny, record, replay, reload

cmake --preset dev && cmake --build --preset dev && ctest --preset dev
tools/format.sh --check                 # clang-format, Google style
tools/tidy.sh                           # clang-tidy
```

Rust 1.95+ (whatever Wasmtime 48 requires), CMake 3.28+, a C++20 compiler.

## Status

**v0.7.0**, pre-1.0: the API can still move between 0.x releases. Both halves
work and are tested end to end in CI. crates.io holds only the `0.0.0`
placeholders that reserve the names, so build from the tag.

See [docs/SPEC.md](docs/SPEC.md) for what is deliberately *not* built,
[CHANGELOG.md](CHANGELOG.md) for what changed and what breaks, and
[docs/adr/](docs/adr/) for the decisions and why.

Licensed under Apache-2.0 WITH LLVM-exception.
