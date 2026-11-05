# PROFILE.md — watoots

| Field | Value |
|---|---|
| Repository | vchance/watoots · default branch `main` · visibility **public** · GitHub plan Free (public repos can have rulesets; none are set today: `branches/main/protection` 404, `rulesets` `[]`, audit `evidence/remote-github.md`) |
| Classification | library (Rust crate + C API, published on crates.io; the `watoots` CLI ships as `watoots-cli`). Milestone M6 (AGENTS.md › Milestones); current release v0.7.1 |
| Accountable owner | Von Chance (vchance) — approver of exceptions and owner-reserved decisions (no merge-acceptance step since core 0.1.9); sole holder of the crates.io token |
| Supported platforms | x86_64 Linux (Ubuntu, `ubuntu-latest`) = `CI` (all four `ci.yml` jobs incl. LeakSanitizer); macOS arm64 = `manual` (the owner's development machine: AGENTS.md › Environment notes, Homebrew LLVM 22, wasi-sdk arm64-macos; no macOS CI job); Windows = `nobody` (no docs claim it, no job builds it); guests: `wasm32-wasip2` Rust guests = `CI`, C++/JS/Python guests = `manual` (CI has only the rustup toolchain; CONTRIBUTING.md › The sample plugins) |
| Toolchain | Rust stable in CI (`dtolnay/rust-toolchain@stable`, no `rust-toolchain.toml`); MSRV `rust-version = "1.95"` (`Cargo.toml`, checked by the `msrv` job); edition 2024; target `wasm32-wasip2`; Wasmtime 48.x; CMake 3.28+ with Ninja and presets `dev` / `asan` / `release` (`CMakePresets.json`); LLVM 22 clang-format/clang-tidy (ADR-0003; CI installs from apt.llvm.org); optional guest toolchains: wit-bindgen-cli + wasi-sdk (C++), ComponentizeJS (JS), componentize-py (Python) |
| Required services for tests | none. Host tests build their components from inline WAT (hermetic); the C++ hosts' ctests and the asset/preview e2e suites need the guests built by `tools/build-plugins.sh` and skip a guest that is not on disk (a skip is not a pass, core R5) |
| CI runners and storage (D9, D11) | public: GitHub-hosted (core D9, owner decision 2026-11-05): public repository, every job names one standard hosted label directly (`ubuntu-latest` for `rust`, `msrv`, `cpp`, `sanitizers`, `rustsec`; `ubuntu-24.04` for the policy workflows) with no placement gate and no `RUNS_ON_*` variable (no placement key (`github.triggering_actor`) is read), so fork pull requests and contributors with no variables still get CI; standard labels only (no larger, paid runners); anyone with write access may run workflows; Actions › Fork pull request workflows from outside collaborators is set by the owner (R8, require approval); the scheduled `audit` lane runs hosted; no self-hosted runner, no Ubicloud, no artifact store (D11: no artifact uploads, no GitHub cache is used: the `rust-cache` step(s) carry the D11 guard and are skipped on hosted runners) |

## Device testing (core D10; simulators and emulators)
| Surface | Test runner | PR fast lane (smoke) | Integration / release (full) | Where it runs |
|---|---|---|---|---|
| none | — | — | — | no mobile surface; D10 does not apply until one is added |

Web surfaces: the in-repo test suite the Components table lists is the browser evidence; no multi-browser matrix (core D10, 0.1.16).

## Components and critical paths
Dependency graph (`crates/*/Cargo.toml`): `watoots-trace`, `watoots-capi` and `watoots-cli` depend on `watoots`; `watoots-cli` also on `watoots-trace`.

| Component (path glob) | Owner | Critical? | Tests that cover it (command) | Consumers (transitive) |
|---|---|---|---|---|
| `crates/host/**` (package `watoots`: engine, manifest, permissions, limits, signature, registry, compile cache, typecheck) | owner | yes (the sandbox boundary; every crate depends on it) | `cargo test --workspace` (any change = full suite); `crates/host/tests/*.rs` incl. `wasip3_shape.rs`, `signing.rs`, `limits.rs`; bench `cargo bench -p watoots --bench boundary` (not a gate) | trace, capi, cli; C/C++ hosts in `examples/`; crates.io users |
| `crates/trace/**` (package `watoots-trace`: trace format, recorder, replay) | owner | yes (replay is the fuzzing oracle, ADR-0008) | `cargo test -p watoots-trace -p watoots-cli` (`tests/record_replay.rs`, `tests/fuzz_properties.rs`) | cli; crates.io users |
| `crates/host-capi/**` (package `watoots-capi`: cbindgen C API, `include/watoots.h` generated and committed, `watoots.hpp`, CMake package) | owner | yes (public C ABI; handle lifetimes) | `cargo test -p watoots-capi`; header-current step; `ctest --preset dev` and `ctest --preset asan` (`crates/host-capi/tests/*.cc`, ABI compile tests) | `examples/host-cpp*`; C/C++ consumers |
| `crates/cli/**` (package `watoots-cli`, binary `watoots`) | owner | user-visible (CLI output) | `cargo test -p watoots-cli` (`tests/cli.rs`); `tools/demo.sh`, `tools/demo-preview.sh` | users; the demos |
| `examples/**` (WIT worlds, ten guests, policies, fixtures, three C++ hosts) | owner | yes for `examples/wit/preview`, `fixtures/preview`, `host-cpp-preview` (the headline untrusted-input path) | `cargo test --workspace` (integration tests use them); cpp + sanitizers jobs (`tools/build-plugins.sh rust rust-asset rust-qoi rust-farbfeld`, ctest dev/asan, demos); non-Rust guests: `manual` (build all four before a change to `examples/wit/asset`, CONTRIBUTING.md) | tests, demos, README |
| `tools/**` (build-plugins, format, tidy, demos, `make-preview-fixtures`, `lib/`) | owner | build/lint path | cpp job (runs format, tidy, both demos) | CI, contributors |
| `CMakeLists.txt`, `CMakePresets.json`, `cmake/**`, `.clang-format`, `.clang-tidy` | owner | build system | cpp + sanitizers jobs | C/C++ consumers |
| `docs/**` (`SPEC.md` = scope source of truth, `SECURITY.md` threat model, `MANIFEST.md`, `adr/` 14 ADRs), `README.md`, `CHANGELOG.md`, crate `README.md`s (each is the crates.io page) | owner | spec/ADR changes are design decisions | none automated beyond `cargo doc` (rustdoc only) | users, crates.io |

## Check selection (impact-based; core D2)
Today `ci.yml` has no `paths` filter and every push to `main` and every pull request runs all four jobs (`rust`, `msrv`,
`cpp`, `sanitizers`); `audit.yml` runs on `Cargo.toml`/`Cargo.lock` changes, weekly and on dispatch. The rows below are
what reviewers use to judge whether the evidence covers a change; the deviation from D2 is recorded under Proposed
D-rule overrides.

| Changed paths | Checks that run on the PR | Evidence recorded as |
|---|---|---|
| `**/*.md` (not `AGENTS.md`, `PROFILE.md`, `CLAUDE.md`; not `docs/SPEC.md` or `docs/adr/**` when they change a decision; not crate READMEs, which are packaged) | PR review; full `ci.yml` runs anyway (no paths filter) | CI per job; build/test effectively `n/a` |
| `AGENTS.md`, `PROFILE.md`, `CLAUDE.md`, `.agent-rules/**`, `.githooks/**`, `.claude/**`, `.github/**` | `policy-drift` + `pr-evidence` + full `ci.yml`; **high tier** (R9) | per check |
| `crates/host/**`, `Cargo.toml`, `Cargo.lock` | **escalation: full `ci.yml`** + `audit` (lockfile/manifest) | per job |
| `crates/trace/**` | target: `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test -p watoots-trace -p watoots-cli` (today: full `ci.yml`) | per command |
| `crates/cli/**` | target: fmt, clippy, `cargo test -p watoots-cli`, `tools/demo.sh`, `tools/demo-preview.sh` (today: full `ci.yml`) | per command |
| `crates/host-capi/**` | target: fmt, clippy, `cargo test -p watoots-capi`, header-current step, cpp + sanitizers jobs (today: full `ci.yml`) | per job |
| `examples/**`, `tools/**`, `CMakeLists.txt`, `CMakePresets.json`, `cmake/**`, `.clang-*` | cpp + sanitizers jobs + `cargo test --workspace`; a change to `examples/wit/asset` or an asset guest also needs all four asset guests built locally (manual with evidence) | per job / pasted output |
| `.github/workflows/ci.yml`, `audit.yml`, toolchain or MSRV change, release candidate, >1 crate touched or uncertain mapping | **escalation: full `ci.yml`** + `audit` (dispatch) + `cargo publish --workspace --dry-run` for a release | per job |

Escalation triggers (any one → full `ci.yml`): `crates/host` (shared by every crate), toolchain/build-system change,
broad refactor, uncertain impact, release candidate, a regression that escaped selection.

## Lanes
Runner note: every job (`ci.yml`, `audit.yml`, `policy-drift`, `pr-evidence`) runs on a GitHub-hosted standard label (`ubuntu-latest` or `ubuntu-24.04`; public repository, core D9, owner decision 2026-11-05): no placement gate and no `RUNS_ON_*` variable.

| Lane | What runs | When | Owner of a failure |
|---|---|---|---|
| PR (fast) | `ci.yml` (all four jobs; target: selected per the table) + `policy-drift` (policy paths) + `pr-evidence` | every PR commit | author |
| Integration batch | `ci.yml` on `main` | every push to main (once per merge); no `concurrency` group, so back-to-back merges each run | merger |
| Scheduled | `audit.yml` (`rustsec/audit-check@v2` against `Cargo.lock`) | weekly, Monday 06:00 UTC, plus lockfile/manifest changes and dispatch | owner |
| Release | `cargo audit` run by hand (gate on its exit code; AGENTS.md › Hard constraints), `cargo publish --workspace --dry-run`, full `ci.yml` green on the release commit | before tagging | owner |
| Manual with evidence | non-Rust guests (`tools/build-plugins.sh` with C++/JS/Python toolchains) and the asset/preview e2e suites against them; macOS build and `ctest --preset dev`; `cargo bench -p watoots --bench boundary` when the call path changes (`docs/PERFORMANCE.md`); longer property runs (`WATOOTS_PROPTEST_CASES=5000 cargo test --workspace`) | before a release and after a change to what they cover; pasted with date, SHA, operator | owner |

## Review triggers
| Playbook | Triggered by paths/changes |
|---|---|
| ux-review | CLI output a person reads (`crates/cli/src/**` messages, `inspect` capability summary), `README.md` and crate READMEs (crates.io pages), `docs/WRITING-A-PLUGIN.md`; the previewer's printed output (`examples/host-cpp-preview/**`). No GUI |
| technical-design | new public Rust or C API (`crates/host/src/lib.rs` exports, `crates/host-capi/src/**`, `include/*`), trace format (`crates/trace/**`), manifest/policy schema (`crates/host/src/manifest.rs`, `docs/MANIFEST.md`), any `docs/SPEC.md` "Open decisions" item (ADR in `docs/adr/`) |
| architecture | Wasmtime major or WASI line change (ADR-0013), new major dependency, the C API/CMake package shape (ADR-0005), record/replay model (ADR-0010, ADR-0011) |
| security | `crates/host/src/{manifest,imports,signature,epoch,plugin,host}.rs` (permissions, default-deny, limits, signature at load), `crates/host-capi/**` (handle lifetimes, ABI), `examples/host-cpp-preview/**` and `examples/fixtures/preview/**` (untrusted input), `Cargo.lock` / Wasmtime bumps, `docs/SECURITY.md`, `SECURITY.md`, `.github/**` |
| performance | the call path (`crates/host/src/{plugin,wave,typecheck,compile_cache,profile}.rs`, `crates/host-capi/src/value.rs`); budgets: shape of `docs/PERFORMANCE.md`, measured by `cargo bench -p watoots --bench boundary` on the owner's machine (shared-runner numbers are noise, CONTRIBUTING.md) |
| observability | audit trail (`crates/host/src/audit.rs`, ADR-0011), `wasi:logging` (ADR-0006), profiler (`profile.rs`, ADR-0009), stderr warnings such as `loaded-unverified` |

## Merge authority
- Low/Standard: `.agent-rules/merge.sh <n>` once core R3–R6 hold (D5).
- High: `.agent-rules/merge.sh <n>` once core R3–R6 hold and the independent review verdict `ready` for the head is recorded (D5); no owner acceptance step (owner decision 2026-11-01). Emergency (e.g. a Wasmtime advisory fix): declared only by the owner (vchance), never by the agent; the PR names the declarer; follow-up PR within 7 days.
- Server-side enforcement available: **rulesets are available (public repo) but none are set.** Owner action (audit F2):
  add a `main` ruleset requiring a PR and the `rust`, `msrv`, `cpp`, `sanitizers` checks, blocking force-push and
  deletion. Until then the merge wrapper, the pre-push hook and the two policy workflows are the only mechanisms.
- Owner action (audit `evidence/remote-github.md`): secret scanning, push protection and Dependabot security updates are
  all disabled on this public repo. Not changed by this PR.

## Exceptions (governed)
R-rule gaps only (core §1): what the repository cannot yet satisfy, recorded with authority and expiry. A deviation from a
D-rule is an override, not an exception: in force only as an `overrides: Dn` block in `AGENTS.md › Overrides` (core §5.2).
| Id | Rule | Replacement | Rationale | Authority | Scope | Expiry/review |
|---|---|---|---|---|---|---|
| X1 | R5 (coverage of supported platforms and guests) | macOS and the C++/JS/Python guests are verified by hand (Lanes › Manual with evidence); in CI the e2e cases for absent guests return early inside the test (`crates/host/tests/asset_e2e.rs:26`, `:1281`), so `cargo test` reports them `ok`; whoever records the evidence writes those guests as `skipped`, never as passed | CI has only the rustup toolchain and no macOS job (CONTRIBUTING.md › The sample plugins) | proposed — owner decision | `examples/plugins/{cpp,js,py}-*`, `crates/host/tests/{asset,preview}_e2e.rs`, macOS | review 2027-01-31 |

### Proposed D-rule overrides (not in force)
Moved into `AGENTS.md › Overrides` only once the owner accepts it; until then the core default stands as written.
- proposed `overrides: D2` — every push and PR runs all four `ci.yml` jobs (no `paths` filter, no per-crate selection) — why: ≈ 11 job-minutes per run on free public-repo runners; selection would save little — authority: owner decision pending — scope: `.github/workflows/ci.yml` — review: 2027-01-31

## Release and operations (core R11)
| Field | Value |
|---|---|
| Versioning | semver, workspace `version` in `Cargo.toml` (`0.7.1`) plus the four `watoots* = { version = ... }` path dependencies, the README banner and `SECURITY.md`'s supported line; annotated tags `v0.0.0`–`v0.7.1`; the bump is the owner's |
| Changelog / release notes | `CHANGELOG.md` (Keep a Changelog); work after a tag goes under `[Unreleased]`, never under a shipped version (AGENTS.md › Milestones) |
| Release procedure | CONTRIBUTING.md › Releasing: move `[Unreleased]` under a dated heading; bump versions; `cargo publish --workspace --dry-run`; commit, `git tag -a vX.Y.Z`, push both; **owner-only:** `cargo publish --workspace` (publishes `watoots`, `watoots-trace`, `watoots-capi`, `watoots-cli` in order; needs Von's token). Run `cargo audit` and gate on its exit code first (the v0.7.1 incident). Contradiction to resolve: step 4 pushes the release commit straight to `main`, which core R3 and `.githooks/pre-push` now refuse; under the core the release commit goes through a PR and the tag is created on the merge commit (tags are not blocked by the hook). Owner to update CONTRIBUTING.md |
| Deploy targets | crates.io (four crates, v0.7.1 first published 2026-10-16; `0.0.0` placeholders yanked); GitHub tags. No service deploy |
| Rollback | no unpublish on crates.io: `cargo yank` the bad version (owner) and ship a patch release from `main` (no backport branches, `SECURITY.md`); for an unreleased change, revert the PR |
| Post-release watch | `audit.yml` weekly plus Wasmtime 48.x advisories; GitHub issues and `SECURITY.md` reports; docs.rs build of the published crates. No duration set: proposed — owner decision |
| Dependency cadence | weekly `audit.yml` (in force); Wasmtime 48.x patches taken promptly, majors need an ADR (AGENTS.md › Hard constraints); Dependabot not enabled (owner action) |
| Deprecation policy | none written; pre-1.0 and the API can move (CONTRIBUTING.md); breaking changes are called out in `CHANGELOG.md` (e.g. ADR-0012). Proposed — owner decision: one minor release of deprecation for C API symbols |

## Cleanup (core R10)
| Resource this repo's work creates | Where | Teardown command | Safe to remove when |
|---|---|---|---|
| Build output | `target/`, `build/dev`, `build/asan`, `build/release`, `tools/*/target`, `examples/plugins/*/target`, `*.wasm`, `bindings/`, `node_modules/`, `.venv/` (all gitignored) | `cargo clean`; `rm -rf build/<preset>`; `rm -rf examples/plugins/<guest>/target` | always (rebuilds; `examples/plugins/*/*.wasm` are needed by the e2e tests until rebuilt) |
| Test scratch | `examples/plugins/*-asset/luts/scratch-*.lut` (left by a killed `asset_e2e` run) | `rm examples/plugins/*-asset/luts/scratch-*.lut` | no test running |
| Fuzz output | `crash-NNN.wave` from `watoots fuzz` | delete, or keep as a regression via `watoots replay --emit-test` | finding triaged |
| Test services / simulators / cloud | none | n/a | n/a |
| Worktrees | `.claude/worktrees/*` | `git worktree remove <path> && git worktree prune` | branch merged, `git status` clean |
| CI caches | `Swatinem/rust-cache` entries | GitHub evicts | n/a |

## Budgets
- CI: ≈ 11 job-minutes per `ci.yml` run (main @ `fc8be12`, 2026-10-22: rust 4.5, cpp 3.8, sanitizers 2.6, msrv 0.4 min, warm cache; these timings predate core 0.1.21, which uses no cache on hosted runners); audit run median ≈ 3 min, CI median 320 s wall over 30 runs (audit `evidence/remote-github.md`). Public repo: hosted minutes are not billed. No timeouts set in `ci.yml`/`audit.yml` (audit F10; follow-up).
- Performance: `docs/PERFORMANCE.md` (boundary bench); a change to the call path re-runs `cargo bench -p watoots --bench boundary` locally.
