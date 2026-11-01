# Playbook: testing, impact-based selection and CI evidence (core R5, R6, D2)

## Select by impact
1. Map changed paths → components (`PROFILE.md › Components`) → tests that cover them → transitive consumers.
   Rust workspaces: `cargo test -p <crate>` for changed crates plus reverse dependents (`cargo tree -i`, or
   `cargo nextest` filtersets `rdeps(<crate>)`; the `determinator` crate automates path→package mapping).
   JS monorepos: `turbo run test --filter=...[origin/main]` or `nx affected`. C/C++: ctest labels or presets per component.
2. Uncertain mapping → escalate to the component's full suite; say so in the PR.
3. Docs-only (D6): doc checks only; the drift check for policy files (R9).

## Reuse evidence
- One CI run per head SHA produces the evidence. Author, code reviewer and specialists read the same run.
- Build once: the build job uploads artifacts; test/lint jobs consume them (or share sccache/rust-cache).
- Key a build cache on **everything that changes the build**, not the lockfile alone, and check what the cache action
  already hashes. `Swatinem/rust-cache` hashes the lockfile, toolchain files, every member crate's `Cargo.toml`,
  `.cargo/config.toml` and `CARGO*`/`RUST*` env vars, but **not a virtual workspace's root `Cargo.toml`** (it is not a
  member), which is where `[profile]` lives; and it saves only on a key miss. So in a virtual workspace a profile change
  leaves every job restoring a stale build, rebuilding, and logging "Cache up-to-date" without saving (found in
  historian, 2026-11-01). There, add `hashFiles('Cargo.toml')` to the key (in `shared-key` when that is set: it makes
  rust-cache ignore `key`). A single-package repo needs nothing: its root is a member and already hashed. Confirm from the
  logs: a miss then "Cache saved", and a re-run of the same SHA restoring the new key.
- A Docker layer cache (`type=gha`) rebuilt every layer when the build context was **unchanged** (a cache-restored stage,
  then `COPY --from=` missed; seen in historian, cause not established). Skip the image build when none of its inputs
  changed, comparing with the PR base, or on main with the last *successful* main run. Details for Ubicloud runners:
  `docs/adopting-ubicloud-runners.md` › Caching in the agent-rules repository (private; not copied into this repo).
- Cancel superseded runs (`concurrency: {group: pr-${{ github.event.pull_request.number }}, cancel-in-progress: true}`).

## Revalidate the delta
- A new commit re-runs the checks whose inputs it touched; earlier passes for untouched components remain valid **only if**
  base branch, lockfiles, toolchain and CI config are unchanged. Otherwise re-run.
- Never reuse a pass by PR number alone. Evidence is keyed by SHA.

## Lanes (profile defines them)
- PR fast lane: selected checks, target < 10 min.
- Integration batch: full suite **once per merge to main** (push trigger), not once per PR commit. If merges are frequent,
  batch (run on a schedule every N hours when main moved) and attribute failures by bisect.
- Scheduled: dependency advisories (time-dependent, so they must run on unchanged code), platform matrix, soak/fuzz.
- Release: full matrix + packaging + manual oracles with recorded evidence.
- Manual with evidence: result pasted with date, revision, operator.

## Record
Each check: passed / failed / skipped / unavailable / n/a + SHA + link. Zero tests discovered = failed. A job skipped by
`if:` reports success on GitHub: treat it as `skipped` in the PR body. A workflow skipped by path filter leaves checks
pending: do not list it as a required check unless a no-op job reports for it.

## Anti-gaming
- Deleting or `#[ignore]`-ing a test, lowering a threshold, widening a path filter, or adding `continue-on-error` is a
  High-tier change (R9/safeguard reduction) and must be stated in the classification.
