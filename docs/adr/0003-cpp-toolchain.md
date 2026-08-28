# ADR-0003 — C++ standard, style, testing, and lint

Date: 2026-08-28. Status: accepted.

## Context

The C API is a v0.1 deliverable, not a follow-on (`docs/SPEC.md`), and
`include/watoots.hpp` is shipped to consumers. Its language standard is
therefore not a private choice: whatever it requires is imposed on every
downstream host application. The audience the spec is aiming at — "the *next*
app, and every C++ app, which has no option today" — skews toward large,
conservative codebases.

Nothing in the tree constrained the C++ side yet: no standard, no style, no
test framework, no lint.

## Decision

**C++20 is the floor for the shipped headers.** C++20 supplies what this API
actually wants (`std::span` for byte buffers and `Val` arrays, designated
initializers for config structs) and is available on GCC 11+, Clang 15+, and
MSVC 19.29+. C++23's one real draw here is `std::expected`, and we get it
without raising the floor:

    template <class T> using Result = std::expected<T, Error>;   // C++23
    template <class T> class Result { /* same subset */ };       // C++20

`wt::Result<T>` is `std::expected<T, Error>` where `__cpp_lib_expected` is
defined, and a small shim with the same member names otherwise. The shim
deliberately uses std's `snake_case` spellings and an implicit value
constructor — diverging would defeat the point, since code must compile
unchanged against either — so it carries a `NOLINT` block for the naming and
`google-explicit-constructor` checks.

The cost is real and is documented in the header: `Result` is a *different
type* at C++20 and C++23, so a project mixing standards across translation
units and passing a `Result` between them has an ODR violation. The supported
fix is `WATOOTS_FORCE_RESULT_SHIM`, which selects the shim at any standard.

**Google C++ style**, via `.clang-format` (`BasedOnStyle: Google`). Two of its
rules are design decisions rather than formatting:

- *No exceptions.* This fits: the C API is status-code based, so every fallible
  operation returns `Result<T>`. Google's own answer is `absl::StatusOr`; we
  will not put an Abseil dependency in a public header.
- *Naming.* `wt::Host`, methods `CamelCase`, members `handle_`, constants `kFoo`
  — including accessors, which Google permits to be variable-style but which we
  keep CamelCase because that is the half clang-tidy can enforce. The C surface
  stays `wt_host_new` per ADR-0001; `watoots.h` is the single file that opts out
  of the naming check, alongside the C++-only checks a C header cannot satisfy
  (`enum class`, `using`, and shrinking the enum base type, which would change
  the ABI).

We do not carry per-file license boilerplate, matching the existing Rust
sources; the licence lives in `LICENSE-*` at the root and per crate.

**GoogleTest + GoogleMock** for the C++ suite, pinned by commit via
`FetchContent` and driven by `ctest`. The suite is small by design — the logic
lives in Rust — and targets what actually breaks at an FFI boundary:

1. handle lifetime and move semantics (`wt::internal::OwnedHandle`),
2. error propagation and the `Result` contract,
3. header hygiene: `watoots.h` must compile as C11 and `watoots.hpp` at its own
   C++20 floor, both under `-Wall -Wextra -Wpedantic -Werror`,
4. from M3, an integration test where the C++ host loads a component and is
   denied a permission.

The suite is built once per configuration a consumer can be in — `cxx20`
(the floor, and the only one that exercises the shim), `cxx23` (`std::expected`),
and `cxx23_shim` (forced opt-out) — because `wt::Result` is a different type in
each. CMake probes for `std::expected` independently and tells each target which
backend it should get; a test asserts the header agreed.

`CMAKE_CXX_SCAN_FOR_MODULES` is off. We ship headers, not modules, and leaving
scanning on writes `@...modmap` arguments into `compile_commands.json` that
exist only after a build, which breaks clang-tidy on a fresh tree.

Sanitizer presets (`--preset asan`) build with ASan and UBSan. LeakSanitizer is
Linux-only, so leak checking happens in CI rather than locally on macOS.

**clang-tidy** with `WarningsAsErrors: '*'`, from `bugprone`, `cert`,
`clang-analyzer`, `concurrency`, `cppcoreguidelines`, `google`, `misc`,
`modernize`, `performance`, `portability`, and `readability`, plus
`readability-identifier-naming` configured to Google's scheme so naming is
machine-checked rather than reviewed. `cppcoreguidelines-pro-type-reinterpret-cast`
and the pointer-arithmetic checks stay *on*: wrapping a C API needs those casts,
but each site should carry an explicit `NOLINT`, which keeps the unsafe surface
of the binding countable.

`tools/format.sh` and `tools/tidy.sh` wrap both. `tidy.sh` exists because
clang-tidy is unusable on macOS otherwise: Homebrew's llvm formula is keg-only
so the binary is not on `PATH`, and it parses with its own clang, which is a
different major version from the Apple clang CMake picked — it then cannot find
the standard library and reports `'string' file not found` plus a cascade of
false "field not initialized" errors. The script configures a separate build
tree with the compilers that ship alongside the clang-tidy it found.

On the Rust side, for symmetry: `#[test]` next to code as `CLAUDE.md` already
requires, `cargo-nextest` as the runner, and `insta` snapshot tests for the WAVE
trace format and the `inspect` grant list — both are text renderings, which is
where snapshots earn their keep. `proptest` for binary↔WAVE round-tripping when
M4 lands. None of that is wired up yet; the crates are still empty.

## Consequences

- Consumers need a C++20 compiler and CMake 3.28+. Consumers already on C++23
  get `std::expected` with no action and no separate header.
- We own and must maintain the shim. It is covered by the `cxx20` variant on
  every run, which is the configuration the floor actually promises.
- Configuring the tests needs network access the first time, for GoogleTest.
- Public API review has a mechanical part: naming, const-correctness, and
  member init are enforced, so review can be about the boundary design.

## Still open

How cargo is driven from CMake — Corrosion, a custom command, or building the
staticlib out of band and pointing CMake at it — is unresolved and deliberately
not answered here. The CMake tree is header-only until M3 needs it.
