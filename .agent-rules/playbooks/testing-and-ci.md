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
   - the repository scripts the workflow runs from outside `.github/**` (those under it are CI configuration: below);
   - lockfiles, toolchain files and build-system files (a repository-wide one, such as the root lockfile, reaches every
     job; one that only a single component's build reads, such as that app's Gradle lockfile, reaches that component);
   - shared or widely consumed code the profile names (map it by its consumers, traced from the build files and tests
     that read it, including reads across components such as a test that opens another component's source).

   These always set `all`:
   - a file matching no component and no "no product job" pattern (unknown paths escalate; they never skip);
   - a file list that is empty or could not be computed (a non-zero exit, a missing merge base);
   - release tags, `workflow_dispatch`, `merge_group`, and the scheduled integration run when the default branch moved
     (item 7);
   - a push to the default branch whose previous revision cannot be read (`github.event.before` all zeros, or not
     fetchable). Otherwise a push selects from `git diff --name-only --no-renames "$BEFORE" "$AFTER"` after
     `git fetch --depth=1 origin "$BEFORE"` (the diff needs only the two trees; after a force-push it is still the
     right tree diff while `before` can be fetched), exactly as a PR does.

   A mapping narrower than `all` for an escalation file is recorded in the profile with the trace that justifies it, and
   the PR that introduces it lists it as a safeguard change (item 8). Prefer `all` unless the trace is short and checkable
   (the build files and tests that read the file), or comes from a dependency graph the build tool reports. Such a trace
   can go stale without any `.github/**` edit, so item 5 makes every review check it.

   Paths that are known to need no product job (prose under D6, say) are listed as such, so a Markdown edit does not
   escalate; they are a pattern in the job like any component, not a default. The agent-policy files are in that list
   (core D2, since 0.1.18): `AGENTS.md` and `CLAUDE.md` at any depth, `PROFILE.md`, `.agent-rules/**`, `.claude/**`,
   `.githooks/**` and `.github/pull_request_template.md`, the set `policy-drift.yml` runs on, less the workflows (an
   adapter for another tool, `.codex/**` or `.cursor/**`, joins the set in the PR that adds it to `policy-drift.yml`).
   In the ordinary case no build, test or workflow step reads them (`merge.sh`, the pre-push hook and `check.py` run on
   a developer's machine or in `policy-drift`), their checks are `policy-drift` and `pr-evidence`, and R9 keeps every
   change to them High tier with an independent review, so a re-pin that changes only these files costs no product
   runner-minutes. **Check what reads them before listing them**: a repository-wide check that walks `git ls-files` (a
   spelling or grammar scan, a link checker, a license-header check) reads them too; then the profile maps the set to
   that job only, with the trace (the script and the line that lists the files), and that job alone runs for them.
   Selection still fails closed: the set is never "no component", it is a pattern whose job list is empty or exactly
   the readers the profile names.

   **The CI configuration** (since 0.1.22, owner decision 2026-11-05) is a second such pattern: the rest of
   `.github/**`, meaning the workflows, composite actions and the selection script, toolchain pins and build flags set
   there included. Its checks are `policy-drift` (whose `check.py` lints the D9 placement gate and the D11 cache and
   artifact rules, not the jobs' logic) and `pr-evidence`; the `changes` job and `ci-result` still run. What that gives
   up, stated in core D2: the changed workflow's product jobs first run on the default branch, in the scheduled
   integration run (item 7), because the merge's push run selects nothing for it either; jobs that run only on release
   refs, `merge_group`, a deploy or `workflow_dispatch`, a workflow with no `schedule` trigger, and the scheduled lane's
   own definition are first exercised when next needed; the selection script's fail-closed path is not exercised by the
   PR that changes it. A `workflow_dispatch` run on the PR branch runs the changed workflow and every job before merge,
   when someone wants that. A PR that changes a workflow and product paths runs the product paths' jobs under the
   changed workflow.

   A repository that has not adopted the `changes` job yet and selects with workflow-level `paths:` filters negates both
   sets there (`'!.agent-rules/**'`, `'!.claude/**'`, `'!.githooks/**'`, `'!.github/**'` beside its `'!**.md'`, which
   already covers the Markdown members), unless a job reads them: a product run those files trigger can only be waited
   for, since `merge.sh` refuses a canceled check.
   Copy-ready workflow-level list (use it as `paths-ignore:` under `pull_request:` and `push:`, or as the "no product job"
   pattern in the `changes` job; add the Markdown pattern your repository already uses):
   ```yaml
   paths-ignore:
     - 'AGENTS.md'
     - '**/AGENTS.md'
     - 'CLAUDE.md'
     - '**/CLAUDE.md'
     - 'PROFILE.md'
     - '.agent-rules/**'
     - '.claude/**'
     - '.githooks/**'
     - '.github/**'
   ```
   `paths-ignore` and the `ci-result` gate do not mix (see Record): a PR the filter skips never provides `ci-result`, which `merge.sh` requires. Use this list as a workflow-level filter only where the repository has no `ci-result` (since 0.1.21 that means a workflow whose gated jobs are all `# d9-opt-in`, or one with no gated jobs; `check.py` flags a filter beside a `ci-result` job). Where it has one, put the same patterns in the `changes` job as the pattern that selects nothing, and drop the filter in the same PR.
3. **Each product job** lists `changes` in its own `needs:` (a job reads `needs.changes.outputs.*` only from a direct
   dependency, so a job that also needs `build` has `needs: [changes, build]`) and has
   `if: needs.changes.outputs.all == 'true' || needs.changes.outputs.<c> == 'true'` for every component it covers. A
   shared job that others consume (a build) runs on the union of its dependents' components. `always()` is never used to
   bypass a failed dependency.
4. **One `ci-result` job** (job id and name both `ci-result`, so the check is named that) with `needs:` listing every job,
   `changes` included, and `if: always()`. It fails when any needed job's result is `failure` or `cancelled`, so a failed
   `changes` job (whose dependents are all skipped) fails it, and passes when the rest are `success` or `skipped`.
   `merge.sh` requires a `ci-result` check whenever the head's workflows define that job; where the plan allows a ruleset,
   make it the required context there too, so a skipped job never leaves a required check pending. A ruleset does not
   enforce placement, though: GitHub counts a job skipped by `if:` as passing, so only `merge.sh` refuses a skipped `ci-result` (core D9). Policy workflows
   (`policy-drift`, `pr-evidence`) stay as they are.
   **With D9 placement (0.1.21) `ci-result` is also where an unplaced person is caught**: a job skipped because the triggering
   person has no `RUNS_ON_<CLASS>__<login>` looks exactly like a job D2 did not select, and `merge.sh` accepts skipped
   product checks. So `ci-result` itself runs on `SMALL` behind the same gate (unplaced `SMALL` = `ci-result` skipped =
   `merge.sh` refuses), reads the placement of every other required class, and fails when one is missing:
   ```yaml
     ci-result:
       name: ci-result
       needs: [changes, build, test]
       if: ${{ vars[format('RUNS_ON_SMALL__{0}', github.triggering_actor)] != '' && (always()) }}
       runs-on: ${{ fromJSON(vars[format('RUNS_ON_SMALL__{0}', github.triggering_actor)] || '["unplaced"]') }}
       env:
         NEEDS: ${{ toJSON(needs) }}
         PLACED_MEDIUM: ${{ vars[format('RUNS_ON_MEDIUM__{0}', github.triggering_actor)] != '' }}
         PLACED_ARM: ${{ vars[format('RUNS_ON_ARM__{0}', github.triggering_actor)] != '' }}   # listed `known unplaced: ARM` in PROFILE
       steps:
         - run: |
             [ "$PLACED_MEDIUM" = true ] || { echo "::error::no RUNS_ON_MEDIUM__<login> for the triggering person: MEDIUM jobs were skipped, not passed"; exit 1; }
             [ "$PLACED_ARM" = true ] || echo "::warning::no RUNS_ON_ARM__<login>: ARM jobs skipped (known unplaced, PROFILE CI runners row)"
             python3 -c 'import json,os,sys; n=json.loads(os.environ["NEEDS"]); sys.exit(1 if [k for k,v in n.items() if v["result"] in ("failure","cancelled")] else 0)'
   ```
   One `PLACED_<CLASS>` line per required class other than `SMALL`: an `::error::` and `exit 1`, or, for a class the `CI runners`
   row lists as `known unplaced: <CLASS>` (owner decision 2026-11-05: a missing runner that is known is fine), an `::warning::`
   only; the PR evidence records those jobs `skipped: no placement`. A public repository (`public: GitHub-hosted`) places
   nothing and needs none of this (core D9). The in-repo lint requires this: a `pull_request`
   workflow with a gated job that is not opt-in must define `ci-result`, and `ci-result` must mention `RUNS_ON_<CLASS>__`
   for each such class. An **opt-in** job (a platform nobody has runners for, covered another way, recorded in the profile
   `CI runners` row) ends its `if:` line with `# d9-opt-in` and is excluded from both. Policy workflows (`policy-drift`,
   `pr-evidence`) stay as they are: `merge.sh` requires them to have run and passed.
5. **Record** each job left out as `n/a: not selected (D2)` in the PR body, with the run URL that shows it skipped; a
   skipped job is never a pass (R5). The reviewer checks the mapping against the diff: a changed path whose job did not
   run is a blocking finding unless the profile maps it elsewhere. The reviewer also checks whether the diff adds a reader
   of an escalation file that has a narrowed mapping (a build-file dependency, an import, a test that opens a path in
   another component); if it does, the same PR widens the mapping (High by R9), or the finding is blocking. For a change
   to `ci-result` the reviewer checks when it prints `tested-tree` (item 6) and that it echoes no PR-controlled text.
6. **A push carries its pull request's result when the trees match** (core D2, since 0.1.23). The PR run publishes the
   tree it tested; the push run looks for it and carries the result instead of re-running the same content. Tree identity
   covers repository content only: secrets, `vars`, time, external services, base and runner images and advisories are
   covered by the integration lane and the flake rule, not by the carry.
   - **When the PR run publishes.** `ci-result`, as its last step, prints
     `echo "::notice title=tested-tree::$(git rev-parse HEAD^{tree})"` (the checkout is the merge ref, so this is the
     merged tree, not the PR head's) only when every job the selection chose ended `success` (none skipped for placement,
     none failed or canceled) and the PR changes nothing under `.github/**` (a PR that changes the CI configuration, its
     scripts included, never carries; it selects no product job anyway, 0.1.22). A run that selected no
     product job may publish too: the push of an equal tree selects the same nothing. No other job prints a
     `tested-tree` notice, and `ci-result` never echoes PR-controlled text (titles, branch names, test output).
   - **How the push run finds it.** The `changes` job starts with every output set to "select" (item 1) and sets
     `carried` only on an explicit match, so any failed command fails closed. It:
     1. finds the merged PR: `gh api repos/$GITHUB_REPOSITORY/commits/$GITHUB_SHA/pulls`, keeping the one with
        `merged_at` set and `merge_commit_sha == $GITHUB_SHA` (retry twice, 5 seconds apart, when the list is empty);
     2. takes that PR's final `head.sha` (for a merge commit, also check it equals the pushed commit's second parent);
     3. lists runs on exactly that commit: `gh run list --workflow <file> --commit <head.sha> --event pull_request --status success --limit 50 --json databaseId,url,updatedAt`,
        keeping runs that completed within the last 7 days;
     4. in each, takes the `ci-result` job id (`gh run view <id> --json jobs`, `databaseId` of the job named `ci-result`,
        which is its check-run id) and reads `gh api --paginate "repos/$GITHUB_REPOSITORY/check-runs/<job id>/annotations?per_page=100"`;
        exactly one annotation titled `tested-tree` whose message equals `git rev-parse HEAD^{tree}` is a match (zero,
        several or a different value is no match).
     Match: every product output false, `carried=<run url>`; `ci-result` passes and prints "carried from <run url> (tree
     <tree>)". Anything else selects by the pushed range (item 1).
   - Job-level `permissions: { contents: read, actions: read, checks: read, pull-requests: read }`, `GH_TOKEN: ${{ github.token }}`;
     about 5–10 API calls per push.
   - Record the push run as `carried from <run>`; it is never "passed" in its own right (R5) and never release evidence
     (core §4). A rebase or a merge from a stale base changes the tree, so it simply runs.
   - The reviewer of any `.github/**` change checks what `ci-result` prints and when (item 5).
7. **The integration lane: an hourly scheduled run with a 4-hour gate, only when the default branch moved** (owner
   decision 2026-11-04; since 0.1.19 it replaces the full run on every push; since 0.1.23 it survives dropped scheduled runs).
   - Trigger: `schedule: - cron: '23 * * * *'` (an off-hour minute: GitHub delays or drops scheduled runs most at the top
     of the hour) on the default branch, plus `workflow_dispatch` to run it by hand. GitHub may delay or drop any scheduled
     run, and turns schedules off in a public repository after 60 days without activity. A head is covered within about 5
     hours however many runs are dropped; a run that decides to do nothing costs only the `changes` job, on the smallest
     class (about 24 short jobs a day). A repository that keeps a 4-hourly cron instead states in its profile how it keeps
     the cadence.
   - **Find the covering run.** The first step of the `changes` job finds the *covering run*: the latest scheduled or
     dispatched run of the same workflow on the default branch that executed its jobs (not a skip run). First look on the
     current head (`gh api "repos/$GITHUB_REPOSITORY/actions/workflows/<file>/runs?head_sha=<head>&status=completed&per_page=100"`):
     keep only `schedule` and `workflow_dispatch` runs whose `head_branch` is the default branch (the API's `event` filter
     takes one value, so filter client-side; the head's `push` and `pull_request` runs never count) and take the latest by
     start time among those whose product jobs did not all end `skipped`. A covering run there answers the success and
     failure cases below without a long scan, however quiet the branch has been. Otherwise page newest-first through at least 200 runs
     (`gh run list -R "$GITHUB_REPOSITORY" --workflow <file> --branch <default> --status completed --limit 200 --json databaseId,event,headSha,conclusion,startedAt,displayTitle`,
     or more pages; an hourly lane makes about 24 runs a day)
     (keep `schedule` and `workflow_dispatch` events) and stop at the first run whose product jobs did not all end
     `skipped` (`gh run view <id> --json jobs`); a list with no covering run means run. (`run-name` cannot carry the
     decision: GitHub sets it before any job runs. The `changes` job may also print a `lane-decision` annotation to make
     the log readable, but the job conclusions are what count.) Job-level
     `permissions: { contents: read, actions: read }` and `GH_TOKEN: ${{ github.token }}`. Then:
     - covering run on the current head with conclusion `success`: set every output false; every product job is skipped,
       `ci-result` passes, and the log says "covered by run <id> on <sha>";
     - covering run on the current head with conclusion `failure`: run nothing and fail `ci-result` with "carrying the
       failure of run <id> on <sha>", so the failure stays the latest result until a merge moves the head;
     - **the 4-hour gate:** the head has moved, the covering run *succeeded*, and it *started* less than 4 hours ago: run
       nothing and pass with "lane gate: covering run <id> started <time>" (not evidence of anything);
     - anything else (the head moved and the covering run failed, was `cancelled`, `timed_out` or `startup_failure`; it
       started 4 or more hours ago; no covering run; an API error): set `all`, at once. The lane fails toward running, and
       a failure is never held back by the gate.

     A skip, gate or carried run is not evidence for the release gate (core §4): that needs a run that executed the jobs
     on the revision.
   - The run covers every merge since the previous covering run at once. When it fails in a job some of those PRs
     skipped, bisect over them (`git log <previous>..<head>`): run the failing job's command locally on each candidate,
     or push a temporary branch at the candidate SHA and dispatch the workflow on it (`workflow_dispatch` takes a branch
     or tag, not a SHA, and runs every job), deleting the branch afterward (R10). Fix the mapping in the same follow-up PR
     as the code fix and say so: the miss is evidence that the selection was too narrow, never a reason to skip the run.
   - **Staleness check.** The agent checks at session start (and the policy-drift job may also) and warns when the default branch's newest
     commit is more than about 8 hours newer than the head of the latest covering run. With the hourly scheduled run and gate this
     is the alarm that the backup did not work (schedules disabled, every scheduled run failing), not the main safeguard.
   - Every non-PR run completes: give non-PR runs their own concurrency group (for example keyed on `github.run_id`),
     because with `cancel-in-progress: false` GitHub still cancels a *pending* run in the same group when a newer one
     queues.
   - **Deploys.** A deploy that gates on the commit's checks (Render's `checksPass`, a required-checks deploy rule) sees
     the push run, which runs the selected jobs and `ci-result`, not the full suite. A selection miss can therefore reach
     production before the next covering run catches it (up to about 5 hours with the hourly run and gate); a carried push
     run executes no product job at all, so a deploy gated on it sees only its PR run's result. A
     repository that deploys says in its profile which it chose, as an owner decision: accept that exposure (the push
     run is its release gate), gate the deploy on the covering run (deploy from the scheduled run, or dispatch a full run
     before deploying), or turn automatic deploys off and deploy only when the owner says so (the agent then checks that
     a covering run, or a dispatched full run, is green on the revision before deploying it). A release that needs the full suite on its exact revision gets it from a
     release ref or a dispatched run (core §4 Release).
8. **Changing the mapping.** Every edit to it is High by R9 (it is in `.github/**`). Adding paths to a job or adding an
   escalation trigger widens the checks. Removing paths from a job, removing an escalation trigger or the `ci-result`
   gate is also a safeguard reduction, and the classification says so; so is removing or lengthening the integration
   `schedule`, loosening the covered-head comparison, or letting a skip run pass when its covering run did not succeed.
9. **Measure** the change in the PR: runner-minutes (vCPU × minutes) for a typical PR of each component before and after,
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

## Batch fix pushes (guidance)
- Every push to a PR is a full selected run. In a review round, fix every finding locally, run the repository's fast
  local checks (format, lint, the touched tests), then push once; not one push per finding. In historian a PR took two
  to four CI runs where one would have done (2026-11-06).

## Flakes (core D2, since 0.1.23)
- A check that fails and then passes on a re-run of the same revision is a failure the first time (R5). Re-run once at
  most, and only for a test outside the components the PR touches, or after stating in the PR why the failure is
  unrelated to the change; a failure in touched code is investigated, not re-run. Record it in the PR's evidence before
  merge as `flake: <test>, failed in <run url>, passed in <run url>`; a flake on the default branch is recorded as a
  comment on the merged PR, plus the tally.
- The profile names one tally per repository, preferably an issue titled `Flaky tests` (a file such as `docs/flakes.md`
  costs a PR per occurrence); add a dated line per occurrence with the revision and both runs.
- A test that flakes again within the profile's window (default 14 days) is fixed, or quarantined by a stated profile
  change that names the test, why, and the issue that tracks the fix. A quarantine (a skip, an exclude) is a safeguard
  reduction and High wherever it lives. Re-running the same test again and again is not allowed.
- Device tests keep their one automatic retry (`device-testing.md`), recorded the same way.

## After the merge (core D5, since 0.1.23)
- Whoever merges follows the default branch's push run for that merge to its end (`gh run watch` or a background
  poll). A failure there is the merger's: re-run once and record a flake, fix it, or report it to the owner. A failure on
  the default branch does not wait for the integration lane. A push that carried its PR's result (item 6) has little
  to watch; say so and stop. A session that cannot stay until the run ends records in the PR that the push run is
  unwatched and names who follows it (the owner, or the next session in the repository).

## Revalidate the delta
- A new commit re-runs the checks whose inputs it touched; earlier passes for untouched components remain valid **only if**
  base branch, lockfiles, toolchain and CI config are unchanged. Otherwise re-run.
- Never reuse a pass by PR number alone. Evidence is keyed by SHA.

## Runners (D9)
- A workflow names a **runner class**, never a provider or a machine, and **no placement means the job is skipped**: there is no
  hosted default (owner decisions 2026-11-05, `docs/2026-11-05-no-hosted-fallback.md`). Every job carries both lines:
  ```yaml
    build:
      if: ${{ vars[format('RUNS_ON_MEDIUM__{0}', github.triggering_actor)] != '' }}
      runs-on: ${{ fromJSON(vars[format('RUNS_ON_MEDIUM__{0}', github.triggering_actor)] || '["unplaced"]') }}
  ```
  A job with its own condition keeps it behind the gate, in parentheses so `||` inside it cannot escape; `always()`,
  `!cancelled()` and `needs.*` stay valid there (the gate is the leftmost conjunct, the rest is one parenthesized group, one line; the lint rejects `&& a || b`):
  `if: ${{ vars[format('RUNS_ON_MEDIUM__{0}', github.triggering_actor)] != '' && (always() && needs.build.result == 'success') }}`.
  A `workflow_dispatch` size override goes first in both lines, with the input's `default` left empty:
  `if: ${{ (inputs.heavy || vars[format('RUNS_ON_LARGE__{0}', github.triggering_actor)]) != '' }}` and
  `runs-on: ${{ fromJSON(inputs.heavy || vars[format('RUNS_ON_LARGE__{0}', github.triggering_actor)] || '["unplaced"]') }}`.
  Classes (no defaults): `SMALL` (policy-drift, pr-evidence, lint, docs); `MEDIUM` (builds, test suites); `LARGE` (container
  builds, heavy integration); `ARM`; `MACOS` (Xcode, simulators); `WINDOWS` (MSBuild). Use a platform class only where the
  repo has such a job. The in-repo `check.py` (policy-drift) fails on `ubicloud-*`, `self-hosted`, box names, `-latest`,
  `matrix.`-carried runners, block-form `runs-on:` lists, a fallback other than `["unplaced"]`, a job without the leading gate,
  the old `github.actor` key and a PR workflow without `ci-result` (see Selection in the workflow, item 4); `pin.py` lists the
  findings in its notes when a repo is re-pinned, and the re-pin PR rewrites them.
- **Explicit hosted placement.** A person who may use GitHub-hosted runners (and owns the minutes) sets their variable to a hosted
  label, for example `RUNS_ON_MACOS__vchance` = `["macos-15"]`. That is the only way a job reaches a hosted runner. It exists only
  in the variable, which the lint cannot see, so the profile `CI runners` row records each hosted placement with the owner's
  call-out (date, link). Unset = skipped. Never put a hosted label in a workflow file or an input default.
- Placement is **per person**: the variable `RUNS_ON_<CLASS>__<login>` (JSON list of labels, e.g. `["self-hosted","synerlytics"]`
  or `["ubicloud-standard-8"]`) is read for every run that person started **or re-ran** (`github.triggering_actor`; `github.actor`
  stays the original actor on a re-run, so it is not used). Pushes, dispatches, merges, and also editing or labeling someone
  else's PR or re-running their run, run on the triggering person's machines. No variable for that login, and every bot actor
  (`dependabot[bot]`, merge queue) = skipped, and `merge.sh` refuses to merge a PR whose `pr-evidence`, `policy-drift`
  (policy paths changed) or `ci-result` was skipped. **Before a collaborator's first PR can merge they set their variables**
  (they or the owner: variable setting is owner-reserved, R8); a bot's PR is made mergeable by a placed person re-running its
  checks (Actions › Re-run all jobs in each skipped workflow run: pr-evidence, policy-drift, CI; the re-run is attributed to the
  person who clicked) or by closing and reopening it (editing the body re-runs only pr-evidence). An unplaced person, app or bot
  that edits a PR body after the pass re-runs pr-evidence as skipped, which blocks the merge by design (fail closed); a placed
  person re-runs it. A login with `-` cannot be a variable name: that repo keys on `github.actor_id` with
  `RUNS_ON_<CLASS>__ID_{0}` (no re-run placement there: close and reopen the PR instead); the lint fails a mix. Variables are
  repository-level, or organization-level (access "Private repositories", a per-variable setting) where the plan shares them;
  GitHub Free organizations do not share organization variables with private repositories. A public repository never carries a
  self-hosted or paid label (hosted minutes are free there, and a fork PR must never land on a paid or self-hosted runner);
  how its jobs are placed is an owner decision in its profile. Set a variable only where runners with those labels are
  registered (a runner serves the org or repo it was registered in). Only the owner holds the variables permission (it cannot
  be scoped per person), connects Ubicloud and registers runners (R8); an agent proposes the JSON and records the placement in
  the PROFILE `CI runners` row.
- The class form is hygiene, not a control: any writer can put a label in a branch and the job runs before policy-drift fails
  the PR, and the triggering person is not the code's author. A self-hosted pool is reachable by every writer of every repo it
  serves: register such runners ephemeral, in a runner group restricted to selected private repos with public repos off, and
  keep `pull_request_target` / `workflow_run` / secret-bearing jobs off a pool that runs PR code (D9).
- Moving a class is a variable change, no commit, effective on the next run. There is no automatic overflow between pools
  (labels are a conjunction; GitHub has no failover): watch queue time per pool (`gh run list --json` / the Ubicloud dashboard)
  and add runners or move a class when a pool waits.
- **`schedule` and `push` runs.** A `schedule` run is attributed to the user who last modified the cron schedule in the workflow
  file, so the integration lane follows that person's variables; a push to the default branch to whoever merged. If that
  person has no placement the run is all-skipped: green on GitHub, not a failure, not evidence (D2). Say in the profile whose
  lane it is; that operator makes the last change to the cron line, and after the first scheduled run you check it executed
  jobs (`gh run view <id> --json triggeringActor,jobs`). Release needs a covering run that executed (core §4), so an unplaced lane
  blocks a release, not a merge. A checks-gated deploy on merge treats a skipped push run as not passed.
- Facts that still hold from the Ubicloud move: GitHub reports Ubicloud and self-hosted runners as `self-hosted`
  (`runner.environment`), so never gate a step on "hosted only" (persistent builders, cache steps). A label the account cannot
  provide queues forever and never fails: unset or change the variable before decommissioning runners (the jobs are then
  skipped), and keep a break-glass path in the profile (the owner edits their variables; a runner group restricted to
  selected private repos with "Allow public repositories" off). Ubicloud runners are thrown away after each job: install
  prebuilt tools, `apt-get update` before install; the clock is not synchronized and disk latency varies between VMs.
  Artifacts and caches follow D11 (below, § Artifacts and caches (D11)). Self-hosted
  runners are not thrown away: a job must not depend on a clean workspace or leave root-owned files behind. Keep `MACOS` and
  `WINDOWS` jobs to the lanes that need them (hosted macOS minutes cost about ten times Linux). The design notes with the
  measured data live in the agent-rules repository: `docs/2026-11-05-runner-classes.md`, `docs/2026-11-05-no-hosted-fallback.md`.

## Artifacts and caches (D11)
- Placement stays each person's (D9); D11 is decided at run time from what the runner declares. The store host's runners declare
  `CI_ARTIFACT_STORE=/data/ci-artifacts` and `CI_LOCAL_CACHE=/data/sccache` in the runner's `.env` file (an operator task on the
  box; the runner reads `.env` at start; that its entries reach job steps is not in GitHub's docs and is confirmed on the first box run, design doc › Storage). A runner without them is not the box.
- sccache: never set `SCCACHE_GHA_ENABLED` (sccache's GitHub cache backend, a GitHub storage service; `check.py` flags any value
  but an explicit off, in `env:`, an `export` or a `GITHUB_ENV` write; best effort, not exhaustive: review catches other forms). On
  the box point it at the local cache (`SCCACHE_DIR=$CI_LOCAL_CACHE/<org>/<repo>/sccache` after the declaration step).
  `mozilla-actions/sccache-action` installs sccache; it does not enable the GitHub backend by itself.
- GitHub's `env` context "does not contain variables inherited by the runner process" (docs, contexts › env), so a step-level
  `if: env.CI_LOCAL_CACHE == ''` cannot see the `.env` value by itself. The documented way is a first step that copies it into the
  job's env (`GITHUB_ENV`), after which `env.*` is available in every later step's `if` (`env` is in the step-`if` context list).
  `runner.name` would hard-code box names into the repository, which D9 forbids, so it is not used.
  ```yaml
      steps:
        - uses: actions/checkout@<sha>
        - name: Declare what this runner has (D11)
          shell: bash   # bash syntax; a WINDOWS job's default shell is pwsh
          run: echo "CI_LOCAL_CACHE=${CI_LOCAL_CACHE:-}" >> "$GITHUB_ENV"
        - name: Cache (a provider that redirects the cache service off GitHub: Ubicloud; never a hosted runner)
          if: ${{ runner.environment == 'self-hosted' && env.CI_LOCAL_CACHE == '' }}
          uses: actions/cache@<sha>
          with: { path: ~/.cargo/registry, key: cargo-${{ hashFiles('**/Cargo.lock') }} }
        - name: Local cache (the build box)
          if: env.CI_LOCAL_CACHE != ''
          run: |
            mkdir -p "$CI_LOCAL_CACHE/$GITHUB_REPOSITORY/cargo"      # the repo's own subdirectory (org/repo, like the artifact path), never the root
            echo "CARGO_HOME=$CI_LOCAL_CACHE/$GITHUB_REPOSITORY/cargo" >> "$GITHUB_ENV"
  ```
  The same pair applies to setup-action `cache:` inputs (set `cache:` on the guarded variant only), `Swatinem/rust-cache` and Docker
  `type=gha` (`cache-from`/`cache-to` on the guarded variant; sccache/ccache directories point at `$CI_LOCAL_CACHE/<org>/<repo>/…` on the box). A GitHub-hosted runner (explicit placement) never uses GitHub's cache: the `runner.environment == 'self-hosted'` conjunct makes it run uncached. Setup actions that cache by default (setup-go; setup-node v5 with `packageManager`) and `enable-cache:` inputs are checked in review; set `cache: false` on the unguarded variant.
  `check.py` requires both conjuncts on the step's own `if:` line and, per job, the `GITHUB_ENV` declaration before the first use; it cannot see placement.
- Artifact write (never `upload-artifact`; never `mkdir -p` the root; fail loudly where there is no store):
  ```yaml
        - name: Store artifacts (D11)
          run: |
            test -n "$CI_ARTIFACT_STORE" && test -d "$CI_ARTIFACT_STORE" || { echo '::error::no artifact store on this runner (D11)'; exit 1; }
            d="$CI_ARTIFACT_STORE/$GITHUB_REPOSITORY/${GITHUB_RUN_ID}-${GITHUB_RUN_ATTEMPT}/test-reports"
            mkdir -p "$d" && cp -r target/reports/. "$d"/
            echo "stored: $d ($(du -sh "$d" | cut -f1))"
  ```
  Put the path, the exit code and a one-line summary in the evidence row: reviewers and cloud sessions cannot read the box. A job
  handoff between jobs is one job, or both jobs write and read the same run directory on the box (both placed on it).

## Lanes (profile defines them)
- PR fast lane: selected checks, target < 10 min.
- Integration batch: full suite on an **hourly scheduled run with a 4-hour gate, only when the default branch moved**,
  covering the head within about 5 hours (item 7 of § Selection in the workflow); a push to the default branch runs the selected jobs only.
  Attribute a failure by bisecting over the merges since the previous green scheduled run.
- Scheduled: dependency advisories (time-dependent, so they must run on unchanged code), platform matrix, soak/fuzz.
- Device tests (D10, `playbooks/device-testing.md`): PR fast lane runs the smoke subset on one simulator or emulator per
  platform when D2 maps the change to the surface; the integration batch runs every target in the profile; release runs every
  target plus manual oracles.
- Release: every device target + packaging + manual oracles with recorded evidence.
- Manual with evidence: result pasted with date, revision, operator.

## Record
Each check: passed / failed / skipped / unavailable / n/a + SHA + link. Zero tests discovered = failed. A job skipped by
`if:` reports success on GitHub: treat it as `skipped` in the PR body. A workflow skipped by path filter leaves checks
pending: do not list it as a required check unless a no-op job reports for it. The agent-policy files (§ Selection in
the workflow, item 2) are excluded from a workflow-level filter the same way. Workflow-level filters and the `ci-result`
gate do not mix: once a workflow defines `ci-result`, `merge.sh` requires that check on every PR, and a PR the filter
skips can never provide it, so adopt the `changes` job and drop the filter in the same PR. Prefer job-level selection with the
`ci-result` gate (§ Selection in the workflow) to workflow-level `paths:` filters.

## Anti-gaming
- Deleting or `#[ignore]`-ing a test, lowering a threshold, narrowing what a job's path selection covers (or removing an
  escalation trigger or the `ci-result` gate), or adding `continue-on-error` is a High-tier change (R9/safeguard
  reduction) and must be stated in the classification.
