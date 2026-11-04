# Playbook: testing, impact-based selection and CI evidence (core R5, R6, D2, D9, D10)

## Select by impact
1. Map changed paths → components (`PROFILE.md › Components`) → tests that cover them → transitive consumers.
   Rust workspaces: `cargo test -p <crate>` for changed crates plus reverse dependents (`cargo tree -i`, or
   `cargo nextest` filtersets `rdeps(<crate>)`; the `determinator` crate automates path→package mapping).
   JS monorepos: `turbo run test --filter=...[origin/main]` or `nx affected`. C/C++: ctest labels or presets per component.
2. Uncertain mapping → escalate to the component's full suite; say so in the PR.
3. Docs-only (D6): doc checks only; the drift check for policy files (R9).

## Selection in the workflow (D2, since 0.1.17)
Running every job on every PR wastes runner minutes on code the change cannot affect (yuppers, 2026-11-04: an
Android-only PR ran the backend, web, database and container jobs, about 40 runner-minutes for nothing). The
workflow, not the agent, decides what runs, from the same table the profile publishes:

1. **One `changes` job first**, on the small runner. It lists the changed files with renames split into both paths,
   so a file moved out of a component still selects it. On `pull_request`, check out with `fetch-depth: 2` and diff the
   test merge commit against its first parent (`git diff --name-only --no-renames HEAD^1 HEAD`, which equals the
   merge-base diff against the current base), or check out with `fetch-depth: 0` and diff against
   `git merge-base "origin/$BASE_REF" HEAD`, passing the base ref through `env:` rather than an inline expression. Run the
   script with `shell: bash` (which sets `pipefail`) or check each exit status. It sets one boolean output per component
   in `PROFILE.md › Components`, plus `all`. A third-party action (`dorny/paths-filter`) is fine if pinned by commit SHA
   like every action, but on `pull_request` it lists files through the API, which stops at 3000 files (treat a list at
   the cap as `all`), it has no "matched nothing" output (compare its file list with the union of the filters), and
   negated patterns under its default `predicate-quantifier: some` match more than they appear to. Keep the path
   patterns in one place under `.github/**` (in the job, or in a script beside the workflow such as
   `.github/scripts/ci-changes.sh` with its own cases), in the same order as the profile's `Check selection` rows, so
   every edit to the mapping is a `.github/**` change: High by R9, with `policy-drift` required.
2. **Escalation (selection fails closed).** These select every job that reads them, and set `all` when that set
   cannot be stated exactly:
   - the repository scripts the workflow runs;
   - lockfiles, toolchain files and build-system files (a repository-wide one, such as the root lockfile, reaches every
     job; one that only a single component's build reads, such as that app's Gradle lockfile, reaches that component);
   - shared or widely consumed code the profile names (map it by its consumers, traced from the build files and tests
     that read it, including reads across components such as a test that opens another component's source).

   These always set `all`:
   - `.github/**` (workflows, composite actions and the selection script itself);
   - a file matching no component and no "no product job" pattern (unknown paths escalate; they never skip);
   - a file list that is empty or could not be computed (a non-zero exit, a missing merge base);
   - any non-PR event: push to the default branch, release tags, `schedule`, `workflow_dispatch`, `merge_group`.

   A mapping narrower than `all` for an escalation file is recorded in the profile with the trace that justifies it, and
   the PR that introduces it lists it as a safeguard change (item 7). Prefer `all` unless the trace is short and checkable
   (the build files and tests that read the file), or comes from a dependency graph the build tool reports. Such a trace
   can go stale without any `.github/**` edit, so item 5 makes every review check it.

   Paths that are known to need no product job (prose under D6, say) are listed as such, so a Markdown edit does not
   escalate; they are a pattern in the job like any component, not a default.
3. **Each product job** lists `changes` in its own `needs:` (a job reads `needs.changes.outputs.*` only from a direct
   dependency, so a job that also needs `build` has `needs: [changes, build]`) and has
   `if: needs.changes.outputs.all == 'true' || needs.changes.outputs.<c> == 'true'` for every component it covers. A
   shared job that others consume (a build) runs on the union of its dependents' components. `always()` is never used to
   bypass a failed dependency.
4. **One `ci-result` job** (job id and name both `ci-result`, so the check is named that) with `needs:` listing every job,
   `changes` included, and `if: always()`. It fails when any needed job's result is `failure` or `cancelled`, so a failed
   `changes` job (whose dependents are all skipped) fails it, and passes when the rest are `success` or `skipped`.
   `merge.sh` requires a `ci-result` check whenever the head's workflows define that job; where the plan allows a ruleset,
   make it the required context there too, so a skipped job never leaves a required check pending. Policy workflows
   (`policy-drift`, `pr-evidence`) stay as they are.
5. **Record** each job left out as `n/a: not selected (D2)` in the PR body, with the run URL that shows it skipped; a
   skipped job is never a pass (R5). The reviewer checks the mapping against the diff: a changed path whose job did not
   run is a blocking finding unless the profile maps it elsewhere. The reviewer also checks whether the diff adds a reader
   of an escalation file that has a narrowed mapping (a build-file dependency, an import, a test that opens a path in
   another component); if it does, the same PR widens the mapping (High by R9), or the finding is blocking.
6. **Selection misses.** The push run on the default branch runs everything, and every such run completes: give non-PR
   runs their own concurrency group (for example keyed on `github.run_id`), because with `cancel-in-progress: false`
   GitHub still cancels a *pending* run in the same group when a newer one queues. When it fails in a job the PR skipped, fix
   the mapping in the same follow-up PR as the code fix and say so; the miss is evidence that the selection was too
   narrow, never a reason to skip that run.
7. **Changing the mapping.** Every edit to it is High by R9 (it is in `.github/**`). Adding paths to a job or adding an
   escalation trigger widens the checks. Removing paths from a job, removing an escalation trigger or the `ci-result`
   gate is also a safeguard reduction, and the classification says so.
8. **Measure** the change in the PR: runner-minutes (vCPU × minutes) for a typical PR of each component before and after,
   from `gh run view --json jobs`.

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

## Runners (D9)
- Public repositories keep GitHub-hosted runners on version-pinned labels (`ubuntu-24.04`, `macos-15`, `windows-2025`);
  everything below applies to private repositories. Reason: hosted minutes are free for public repos, and a fork PR must
  never run on the account's Ubicloud runners.
- Default labels: `ubicloud-standard-2` for small jobs (policy-drift, pr-evidence, lint, docs), `ubicloud-standard-4` for
  build/test until measured. Make the size a `workflow_dispatch` input with the default as fallback
  (`runs-on: ${{ inputs.heavy || 'ubicloud-standard-4' }}`) so sizes can be measured without a code change; pick the size by
  cost per run (vCPU × rounded-up minutes), wall-clock as tie-breaker, and post the table in the PR.
- Write the labels in the workflow (`runs-on: ${{ inputs.heavy || 'ubicloud-standard-4' }}`); do not route product jobs
  through `RUNS_ON` / `CONTAINER_RUNS_ON` variables (squishhoard's still pointed at self-hosted runners after the switch; the
  owner clears them). The only variable indirection is `fromJSON(vars.RUNS_ON_SMALL || '["ubuntu-24.04"]')` in the two
  policy workflows, so they move when the owner sets the variable; where hosted minutes are refused by billing (Synerlytics)
  the owner sets it per repo or org. Switch once the owner has confirmed the App covers that organization or account (link
  the statement in the PR body; no per-repo re-confirmation); an uncovered label queues forever and never fails.
- GitHub reports Ubicloud runners as `self-hosted` (`runner.environment`). Never gate a step on "hosted only" (cache
  steps, persistent builders); remove such conditions in the switch PR.
- Keep one break-glass path named in the profile (a self-hosted runner left registered, or the owner re-labelling), because
  an Ubicloud outage or exhausted credit queues every job, including the CI on a revert.
- Runners are thrown away after each job: install prebuilt tools, `apt-get update` before install, cache with the normal
  GitHub cache actions (Ubicloud serves them from its own store; `gh cache list` shows nothing). The clock is not
  synchronised and disk latency varies between VMs: grep for wall-clock assertions and `timedatectl` before moving a suite.
- macOS and Windows jobs stay on version-pinned GitHub-hosted labels, listed in the PROFILE `CI runners` row (D9); keep them to the lanes that
  need them (an iOS `.ipa` for App Automate on main and release, not on every PR commit) because hosted macOS minutes cost
  10× Linux. The long form with worked numbers lives in the agent-rules repository: `docs/adopting-ubicloud-runners.md`.

## Lanes (profile defines them)
- PR fast lane: selected checks, target < 10 min.
- Integration batch: full suite **once per merge to main** (push trigger), not once per PR commit. If merges are frequent,
  batch (run on a schedule every N hours when main moved) and attribute failures by bisect.
- Scheduled: dependency advisories (time-dependent, so they must run on unchanged code), platform matrix, soak/fuzz.
- Device tests (D10, `playbooks/device-testing.md`): PR fast lane runs the smoke subset on one simulator or emulator per
  platform when D2 maps the change to the surface; the integration batch runs every target in the profile; release runs every
  target plus manual oracles.
- Release: every device target + packaging + manual oracles with recorded evidence.
- Manual with evidence: result pasted with date, revision, operator.

## Record
Each check: passed / failed / skipped / unavailable / n/a + SHA + link. Zero tests discovered = failed. A job skipped by
`if:` reports success on GitHub: treat it as `skipped` in the PR body. A workflow skipped by path filter leaves checks
pending: do not list it as a required check unless a no-op job reports for it. Prefer job-level selection with the
`ci-result` gate (§ Selection in the workflow) to workflow-level `paths:` filters.

## Anti-gaming
- Deleting or `#[ignore]`-ing a test, lowering a threshold, narrowing what a job's path selection covers (or removing an
  escalation trigger or the `ci-result` gate), or adding `continue-on-error` is a High-tier change (R9/safeguard
  reduction) and must be stated in the classification.
