# ADR-0005 — Driving cargo from CMake

Date: 2026-08-28. Status: accepted.

## Context

`docs/SPEC.md` left this open, and M3 forces it: the C API is a Rust staticlib
that a CMake project has to link.

The thing that makes this smaller than it first looks is who runs cargo. A
consumer installing the watoots CMake package links a **prebuilt** library and
reads committed headers — they never need a Rust toolchain. Cargo runs only
when *we* build. So the choice affects our own build and CI, not our users'.

## Decision

**A custom command, not Corrosion.**

`crates/host-capi/CMakeLists.txt` runs `cargo build -p watoots-capi` through
`add_custom_command`, wraps the resulting `libwatoots_capi.a` in an IMPORTED
target, and exposes `watoots::capi` carrying the headers, the library, and the
platform link flags the Rust runtime needs.

Corrosion is the obvious alternative and is a good project. It loses here on
one point: it would become a dependency of our build system for a problem that
is thirty lines of CMake. We already own the harder half — the target triple
and profile mapping — and a `FetchContent` of a CMake module at configure time
is a network dependency and a version to track for something we can read in
full.

Consequences of doing it by hand, accepted deliberately:

- **Cargo owns incrementality, CMake does not.** The custom command depends on a
  `CONFIGURE_DEPENDS` glob of the Rust sources, which is enough to re-run cargo
  when they change; cargo then decides whether anything needs rebuilding. We do
  not try to model Rust's dependency graph in CMake, because we would get it
  wrong and cargo is already correct.
- **Profile mapping is explicit.** `Debug` builds the `dev` profile, everything
  else builds `release`. Multi-config generators pick their profile at
  configure time rather than per-build; single-config generators (Ninja, our
  preset) do the right thing, and multi-config is documented rather than
  silently wrong.
- **The platform link flags are ours to maintain.** A Rust staticlib does not
  carry its own system dependencies, so the CMake target names them. That list
  is per-platform and will need revisiting when a new one is supported.

## Consequences

- Building the C++ side requires cargo on PATH. `WATOOTS_BUILD_CAPI=OFF` skips
  it, so the header-only tests from ADR-0003 still build without Rust.
- The generated `include/watoots.h` is committed, written by
  `crates/host-capi/build.rs`, so a consumer needs neither cbindgen nor cargo.
  CI catches a stale header with `git diff --exit-code`.
- If the mapping ever grows past what is readable in one file — cross
  compilation, a second Rust artifact — that is the signal to reconsider
  Corrosion, and this decision should be revisited rather than extended.
