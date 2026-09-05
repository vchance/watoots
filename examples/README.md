# Examples

Two WIT worlds. The first is one world in four guest languages, with four
different policies, because the languages do not cost the same. The second is
one language and a plugin that actually uses a capability.

```
wit/lint/lint.wit     the world every lint sample implements
plugins/rust-lint/    Rust,       via wit-bindgen        ~65 KB
plugins/cpp-lint/     C++,        via wasi-sdk           ~724 KB
plugins/js-lint/      JavaScript, via ComponentizeJS     ~12 MB
plugins/py-lint/      Python,     via componentize-py    ~18 MB

wit/asset/asset.wit   an image-pipeline world
plugins/rust-asset/   Rust; the one sample that opens a file itself

policies/             one manifest per plugin
host-cpp/             a C++ host application over the C API
```

`cpp-lint` is the one that closes the loop. This project's claim is that C++
applications have no component-model plugin option today, and a C++ *host* only
demonstrates half of that. Here C++ is the untrusted side, sandboxed by the same
manifest as everything else — and `wit-bindgen` emits C, so it is C bindings
driven from C++, exactly as `watoots.hpp` is a C++ layer over a C API. The
boundary is C in both directions; the language on each side of it is a local
choice.

Build the plugins (each needs its own toolchain; missing ones are skipped):

```sh
tools/build-plugins.sh
```

Then run any of them through the same host binary:

```sh
cmake --preset dev && cmake --build --preset dev
./build/dev/examples/host-cpp/host_cpp \
    examples/plugins/rust-lint/rust_lint.wasm examples/policies/rust-lint.toml
```

All four print byte-identical diagnostics. The host is not recompiled between
them and contains nothing language-specific — the WIT world is the entire
contract.

## What the four policies show

The interesting output is the grant list the host prints before loading
anything. It is derived from the component's own declared imports, so it is
available at install time, before a line of guest code runs.

| | Rust | JavaScript | Python |
|---|---|---|---|
| monotonic clock | yes | yes | yes |
| environment | yes | yes | yes |
| wall clock | — | yes | yes |
| filesystem | — | yes | yes |
| random | — | — | yes |
| socket interfaces | — | — | yes |

None of the plugins *uses* those capabilities: all three do string processing
and call one host function. The difference is what each toolchain links in.
A Rust guest pulls the clock and environment through `std`. StarlingMonkey
needs the wall clock for `Date` and the filesystem for module resolution.
CPython links sockets and seeds its hash randomisation at startup.

Two consequences worth internalising:

- **The import list reflects the toolchain, not the author.** A denial that
  looks wrong is usually the runtime, not the plugin.
- **Importing an interface and being able to use it are different.**
  `py-lint.toml` has `net = []`, which grants the socket *interfaces* CPython
  links while leaving no host reachable — wasmtime-wasi refuses every
  connection. Denying the import outright would refuse CPython altogether;
  pretending an allowlist works would be worse.

`cpp-lint.toml` needs `clocks = "wall"` where `rust-lint.toml` needs only
`monotonic`. Nothing in `lint.cc` asks for the time; wasi-libc links the wall
clock during startup. Four toolchains, four different bills, and none of them
written by the plugin's author — which is the argument for reading `watoots
inspect` rather than guessing.

ComponentizeJS can drop some of its defaults: `js-lint` is built with
`--disable http --disable random --disable fetch-event`, which removes three
grants that would otherwise be required. What remains is what the engine
genuinely needs.

## A note on speed

The `dev` preset builds wasmtime unoptimised, so compiling a 12–18 MB
JavaScript or Python component takes tens of seconds. Use `--preset release`
when timing anything, or set a `cache_dir` on the host — the second load of the
same component reads precompiled machine code instead.

## The second world: a grant that is actually used

The four lint plugins are granted capabilities none of them uses — the whole
point of that table is that the bill comes from the toolchain. `rust-asset` is
the other case. It implements `wit/asset/asset.wit`, an image pipeline of five
operations, and one of them, `lut`, **opens a file itself**:

```sh
tools/build-plugins.sh rust-asset

watoots inspect examples/plugins/rust-asset/rust_asset.wasm \
    -m examples/policies/rust-asset.toml --provide watoots:asset/log
```

```
capabilities
  filesystem   ok     reads ${plugin_dir}/luts
  ...
```

`rust-asset.toml` grants `fs.read = ["${plugin_dir}/luts"]` and nothing more.
Take that one line out and the plugin does not load at all — not "fails when it
reaches for a file", but fails before instantiation, because `wasi:filesystem`
is declared in the component's binary and the intersection check reads it there.

```sh
watoots run examples/plugins/rust-asset/rust_asset.wasm \
    -m examples/policies/rust-asset.toml \
    --answer 'watoots:asset/log@0.1.0#emit=' -c apply -- \
    '{width: 2, height: 2, pixels: [10, 20, 30, 200, 100, 50, 255, 255, 255, 0, 0, 0]}' \
    '[grayscale, lut("examples/plugins/rust-asset/luts/sepia.lut")]'
```

```
ok({width: 2, height: 2, pixels: [24, 22, 17, 168, 149, 116, 255, 255, 239, 0, 0, 0]})
```

Three things in that command are worth pausing on.

**The path is spelled the way the grant is.** WASI preopens a granted directory
under the name it was granted as, and a guest can only reach it through a path
that starts with that name. `${plugin_dir}` here expanded to the relative path
on the command line, so the `lut` argument is relative too. Name the component
by an absolute path and the LUT needs one.

**Point it outside `luts/` and you get an answer, not a crash.** The process
exits zero — the call succeeded and the *plugin* said no, which is a different
event from the host saying no — and the answer carries a reason:

```
err(unreadable({path: "…", reason: "cannot open it: No such file or directory
  (os error 44) (a path outside every granted directory reports as not found,
  so check the manifest and the spelling of the path)"}))
```

The reason is in the returned value, not only in a log line, so a caller with no
log sink can still tell "the manifest does not cover this" from "that file is
not a lookup table".

**The pixels are a contract.** Every operation in `src/lib.rs` documents its
exact arithmetic — fixed-point Rec. 601 luma, `floor(x + 0.5)` rounding,
truncating nearest-neighbour — because three more guest languages are meant to
implement this world byte for byte, and `crates/host/tests/asset_e2e.rs` asserts
values computed by hand from those rules rather than captured from a run.
