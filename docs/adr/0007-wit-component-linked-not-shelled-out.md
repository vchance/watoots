# ADR-0007 — Link `wit-component`, don't shell out to `wasm-tools`

Date: 2026-09-04. Status: accepted.

## Context

`docs/SPEC.md` lists as a v0.2 candidate:

> `watoots inspect plugin.wasm`: human-readable permission manifest from
> imports […], plus `semver-check` and `targets` wrapped.

The grant list landed first. This decides *how* the two `wasm-tools` checks get
wrapped, because there are two ways to wrap a tool and only one of them is
right here.

The rule this project already follows — "we do not reimplement `wasm-tools
component semver-check` or `targets` — call/wrap them" — settles that we don't
rewrite the logic. It does not say whether to call a subprocess or link a
library, and those differ in ways that matter for a project shipping a C API.

What the two checks actually answer:

- **`targets`** — does this component implement the world my application asked
  for? That is the half the import-intersection check does not cover: `check`
  verifies a component's *imports* against the manifest and says nothing about
  whether its *exports* match the world the host intends to call.
- **`semver-check`** — is this WIT package a compatible successor to that one?
  A question about two WIT packages, asked at development or CI time.

## Decision

**Link `wit-component`.** Both are public library functions:

```rust
wit_component::targets(resolve: &Resolve, world: WorldId, component: &[u8]) -> Result<()>
wit_component::semver_check(resolve: Resolve, prev: WorldId, new: WorldId) -> Result<()>
```

### It is not a new dependency

`wit-component 0.254.0` and `wit-parser 0.254.0` are already in the tree,
pulled in by Wasmtime 48. Linking them costs one `Cargo.toml` line.

That line is **pinned to `0.254`, and the pin is load-bearing** for exactly the
reason [ADR-0004](0004-wave-and-dynamic-typing.md) records for `wasm-wave`: a
semver-incompatible version compiles a second `wasmparser`, and its `Resolve`
and `WorldId` are then different types from the ones in the copy Wasmtime uses.
The failure is a type error at best and two disagreeing validators at worst.

### The C API decides it

`targets` is a plugin-*loading* question, so it belongs on `Host`, behind the C
API, next to `inspect`. A library whose behaviour depends on a binary being on
`PATH` is not a library — and a C++ application embedding watoots cannot
reasonably be told to install a Rust toolchain's CLI first. The C API is a v0.1
deliverable rather than a follow-on precisely so that questions like this get
answered once, for both surfaces.

### Three lesser reasons

- **Hermeticity.** `CLAUDE.md` notes that host tests build their components
  from WAT inline "which keeps them hermetic". A subprocess dependency
  reintroduces what that decision removed, in the tests and in CI.
- **Version skew.** Shelling out means whatever `wasm-tools` happens to be
  installed, which can disagree with the `wasmparser` inside Wasmtime 48. This
  session has already paid this bill twice — the `wasm-wave` pin, and a CI
  failure caused by Ubuntu's clang-format being four majors behind
  ([ADR-0003](0003-cpp-toolchain.md)). Linking pins it.
- **Typed errors.** `Result<()>` maps onto `Error` and `wt_status` directly.
  Parsing another tool's stderr into an error code breaks the first time they
  reword a message.

### Where each one lands

They do not go in the same place, because their inputs differ:

- `targets` → `Host::check_targets(wasm, wit_path, world)`, the C API, and
  `watoots inspect --targets <wit> [--world NAME]`. It takes a component, so it
  belongs on `inspect`.
- `semver-check` → `watoots wit semver-check --previous A --current B`, CLI
  only. It takes two WIT packages and no component, so putting it on `inspect`
  would mean a subcommand whose primary argument is unused. The spec's phrasing
  grouped them; their signatures do not.

## Consequences

- The host gains the check it was missing. Until now watoots could tell you a
  plugin asks for nothing it should not, and not whether it implements what you
  are about to call.
- **A second lockstep pin.** Bumping Wasmtime now means moving `wit-component`
  with it, the same maintenance `wasm-wave` already carries. Two is a pattern:
  the next Wasmtime bump should check every `0.254` pin at once.
- Loading WIT from disk becomes something the host library does, via
  `Resolve::push_path` — a filesystem read on a path the *application* chose,
  not the plugin. It is not a guest capability and does not touch the sandbox.
- We inherit `wit-component`'s idea of what conformance means, including its
  error text. That is the point of wrapping rather than reimplementing.

## Addendum — 2026-09-04, on implementation

Two things the decision above did not anticipate, both about the semantics of
the tools rather than how they are called.

**`targets` requires the world to declare every import the component has**, so
a hand-written application world fails against any real guest. A
`wasm32-wasip2` Rust guest links `wasi:io`, `wasi:cli` and `wasi:clocks`
through `std` whether or not its author asked for them — the same fact
`CLAUDE.md` already records about denials, arriving from a new direction.
`examples/wit/lint.wit` declares `import log;` and nothing else, and the
component built from it fails `--targets` against its *own* world. Using the
check therefore means vendoring the WASI WIT package and writing
`include wasi:cli/imports@0.2.x;`. That is `wasm-tools`' semantics and we keep
it rather than inventing a laxer one, but the error now carries a note saying
so, because "missing import named `wasi:io/poll`" is not a self-explaining
diagnostic.

**`semver_check`'s direction is the opposite of the intuitive one.** Its
predicate is that `new` may have *more imports and fewer exports* than `prev`
— `new` is the consumer, `prev` the provider, and the check instantiates one
with the other. Adding an export to `new` is therefore a break, not a
compatible addition. For a plugin world this is the right direction and worth
stating plainly: a host may offer plugins more and demand less of them without
invalidating the plugins already built against it. The CLI's help says this,
because getting it backwards silently inverts the answer.

`semver-check` also turned out to be an opt-in cargo feature of
`wit-component`, which suits the split above: only `crates/cli` enables it, and
`crates/host` links the crate without it.
