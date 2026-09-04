# Contributing

watoots is pre-1.0 and single-maintainer. The API can still move, so the most
useful contribution right now is a report that something is wrong, confusing,
or missing — not necessarily a patch.

Before anything else, read [`docs/SPEC.md`](docs/SPEC.md). It is the source of
truth for scope, and it lists what this project deliberately does **not** build.
A change that contradicts it needs the spec changed first.

For a suspected security issue, do not open an issue: see
[SECURITY.md](SECURITY.md).

## What you need

| | |
|---|---|
| Rust | 1.95 or newer (MSRV, set by Wasmtime 48) |
| `wasm32-wasip2` target | `rustup target add wasm32-wasip2` |
| CMake | 3.28+, and Ninja |
| LLVM | **22**, for clang-format and clang-tidy |

The LLVM version is pinned and it matters. Neither `BasedOnStyle: Google` nor
the check set behind `WarningsAsErrors: '*'` is frozen across LLVM releases, so
an unpinned linter turns somebody else's upstream release into a red build on
code you did not touch. On macOS, `brew install llvm@22`; on Debian or Ubuntu,
install `clang-format-22` and `clang-tidy-22` from
[apt.llvm.org](https://apt.llvm.org) — the distribution archives are usually
several majors behind. `tools/format.sh` and `tools/tidy.sh` find a 22 on their
own, fall back to whatever is on `PATH`, and `format.sh` warns when it falls
back. See [ADR-0003](docs/adr/0003-cpp-toolchain.md).

You do not need `wasm-tools`, `wac`, `cargo-component` or `wit-bindgen-cli`.
The host's own tests build their components from WAT inline so the suite stays
hermetic.

## The gates

CI runs exactly these. Run them before opening a pull request.

```sh
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --workspace

tools/format.sh --check          # clang-format
cmake --preset dev && cmake --build --preset dev && ctest --preset dev
tools/tidy.sh                    # clang-tidy
```

`tools/format.sh` with no argument rewrites in place. `tools/tidy.sh`
reconfigures its own build tree every run: clang-tidy parses with its own
clang, which is a different major from the compiler CMake picked, and a stale
compile database silently drops whole translation units.

There is also `cmake --preset asan` for AddressSanitizer and UBSan.
LeakSanitizer is Linux-only, so leak checking happens in CI rather than on a
Mac.

## The sample plugins

```sh
tools/build-plugins.sh              # everything whose toolchain is present
tools/build-plugins.sh rust         # or name them: rust, js, py
```

Only the Rust guest is built in CI, because it is the only one whose toolchain
comes from `rustup`. JavaScript needs Node and ComponentizeJS, Python needs
`componentize-py`; the script skips what it cannot build and says so.

`tools/demo.sh` runs the whole story end to end — load a plugin, deny a
permission, record a bug, replay it — against a real compiled component.

## Layout

| | |
|---|---|
| `crates/host/` | the core library: `Host`, `Plugin`, manifest, limits, registry, cache |
| `crates/host-capi/` | the C API, the C++ RAII header, the CMake package |
| `crates/trace/` | trace format, recorder, replay runner |
| `crates/cli/` | the `watoots` binary |
| `examples/` | one WIT world, three guest languages, a C++ host application |
| `docs/adr/` | one file per decision |

Tests live next to the code they cover. Integration tests under
`crates/*/tests/` use the sample plugins in `examples/`.

## Conventions

- Rust 2024 edition. C++20 is the floor for shipped headers, Google style, no
  exceptions across the boundary.
- Public Rust API is designed with a C surface in mind: opaque handles, no
  generics across the boundary, error codes plus message strings. If a change
  cannot be expressed in `crates/host-capi`, that is a design problem rather
  than a follow-up.
- `cppcoreguidelines-pro-type-reinterpret-cast` and the pointer-arithmetic
  checks stay on. Wrapping a C API needs those casts; each one carries an
  explicit `NOLINT` naming the reason, which keeps the unsafe surface of the
  binding countable.
- A decision listed under "Open decisions" in the spec gets an ADR in
  `docs/adr/NNNN-title.md` when it is made. Don't pick silently.

## Licence

By contributing you agree that your work is licensed under
**Apache-2.0 WITH LLVM-exception**, the same terms as the rest of the project.
See [LICENSE-APACHE](LICENSE-APACHE) and
[LICENSE-LLVM-EXCEPTION](LICENSE-LLVM-EXCEPTION), and
[ADR-0002](docs/adr/0002-license.md) for why.
