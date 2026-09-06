# Examples

Two WIT worlds. The first is one world in four guest languages, with four
different policies, because the languages do not cost the same. The second is
the same four languages again, over a plugin that actually uses a capability —
and a host application that owns a codec so no plugin has to.

```
wit/lint/lint.wit     the world every lint sample implements
plugins/rust-lint/    Rust,       via wit-bindgen        ~65 KB
plugins/cpp-lint/     C++,        via wasi-sdk           ~724 KB
plugins/js-lint/      JavaScript, via ComponentizeJS     ~12 MB
plugins/py-lint/      Python,     via componentize-py    ~18 MB

wit/asset/asset.wit   an image-pipeline world
plugins/rust-asset/   Rust        \  four guests again, and each one opens a
plugins/cpp-asset/    C++          \ file itself: `lut` is the step that needs
plugins/js-asset/     JavaScript   / a capability, and the manifest is what
plugins/py-asset/     Python      /  decides whether it gets one

policies/             one manifest per plugin
host-cpp/             a C++ host application over the C API
host-cpp-asset/       a second one, for the asset world, that owns the codec
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

## The second host: the application owns the codec

`host-cpp` drives the lint world; `host-cpp-asset` drives this one. Two binaries
rather than one with a switch, because they demonstrate different things.

```sh
cmake --preset dev && cmake --build --preset dev

./build/dev/examples/host-cpp-asset/host_cpp_asset \
    examples/plugins/rust-asset/rust_asset.wasm \
    examples/policies/rust-asset.toml \
    examples/host-cpp-asset/input.png /tmp/out.png \
    resize:32x32,grayscale,lut:examples/plugins/rust-asset/luts/sepia.lut
```

The same command runs any of the four guests — with **that guest's** policy,
which is the whole point of the table above:

```sh
./build/dev/examples/host-cpp-asset/host_cpp_asset \
    examples/plugins/py-asset/py_asset.wasm \
    examples/policies/py-asset.toml \
    examples/host-cpp-asset/input.png /tmp/out-py.png \
    resize:32x32,grayscale,lut:examples/plugins/py-asset/luts/sepia.lut
```

All four write a byte-identical PNG. `ctest --preset dev -R host_cpp_asset` runs
whichever have been built.

`input.png` is 128×128 and synthetic, generated from three rules so it can be
regenerated rather than trusted: `r = x * 255 / 127`, `g = y * 255 / 127`, and
`b` a 16-pixel checkerboard of 0 and 255. The gradients make `gain` and
`grayscale` visible; the hard checker edges are what `resize`'s
nearest-neighbour rule is easiest to check against.

**No file format appears in `asset.wit`, and that is the design.** The host
decodes the PNG to RGB8 with `stb_image`, hands over pixels, and encodes the
answer with `stb_image_write`. Four guest languages therefore agree byte for
byte without four PNG dependencies, and an untrusted plugin never parses a file
format. stb is fetched by `FetchContent` at a pinned commit exactly as
googletest is, and pulled in `SYSTEM`, which is also what keeps its headers out
of `tools/tidy.sh` while `main.cc` stays in.

**`describe` runs first, and the pipeline is routed on it.** Nothing else in the
repository uses `plugin-info`, and this is what it is for: a step the plugin does
not advertise is refused by name, before a single pixel is marshalled.

### What the WAVE round trip costs

Images cross the C API as **WAVE text**. `wt_plugin_call` takes
`const char* const*`; there is no binary path, and adding one is a separate
decision, not something an example gets to make. The honest accounting, from one
run of a 960×960 image through `grayscale, lut` on the Rust guest, built with
`--preset release`:

```
WAVE argument: 12105635 bytes of text for 2764800 bytes of pixels, built in 13.1 ms
apply():
  answered with 13136333 bytes of WAVE in 176.7 ms
  parsed it in 12.3 ms

where the time went (watoots profile)
  wall         51.8 ms
  guest        2.7 ms (5%)
  host calls   0.3 ms (0%)
  marshalling  48.7 ms (93%) -- the pixels, as WAVE text, in both directions
```

Three numbers to take from that.

**Text costs about 4.4× the pixels.** 2.6 MiB of image is 11.5 MiB of argument
and 12.5 MiB of answer. Building it with naive concatenation is slow enough to
notice, so `WaveImage` reserves five characters per byte and appends from a
256-entry table of pre-rendered decimals; that is the difference between 13 ms
and a good deal worse.

**`watoots profile` says 93% marshalling — and still does not see most of it.**
The profile's `wall` is 51.8 ms against the 176.7 ms the application measured
around the same call. `Plugin::call_wave` converts WAVE text to `Val` *before*
the profiled window and back *after* it, so the ~125 ms difference is the text
conversion itself, invisible to the profiler. What the profiler does attribute
to `marshalling` is the canonical ABI's copying, which is real and is 93% of
what it can see. Both numbers are honest; neither is the whole bill.

**There is a hard ceiling, and it is lower than a photograph.** A returned
`list<u8>` is lifted into `Vec<Val>`, and wasmtime charges its per-hostcall data
budget — 128 MiB by default — at `size_of::<Val>()` = 48 bytes *per element*.
So the largest image `apply` can return is 2,796,202 bytes: 965×965 RGB8 works,
966×966 traps with

```
too much data is being copied between the host and the guest:
fuel allocated for hostcalls has been exhausted
```

A 1024×1024 image goes *in* fine — only guest-to-host data is metered — and
cannot come back. That is a property of the dynamic `Val` path, which is the
only path the C API offers; it is not `limits.fuel`, and raising that does not
help.
