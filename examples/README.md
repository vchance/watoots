# Examples

One WIT world, four guest languages, one host — and four different policies,
because the languages do not cost the same.

```
wit/lint.wit          the world every sample implements
plugins/rust-lint/    Rust,       via wit-bindgen        ~65 KB
plugins/cpp-lint/     C++,        via wasi-sdk           ~724 KB
plugins/js-lint/      JavaScript, via ComponentizeJS     ~12 MB
plugins/py-lint/      Python,     via componentize-py    ~18 MB
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
