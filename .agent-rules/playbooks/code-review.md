# Playbook: independent code review (every PR; core R4)

Reviewer input: PR number, the classification block, base/head SHAs. Not the author's conclusions.
Reviewer output: findings with severity, file:line, trigger, impact, confidence, fix; verdict; evidence used.

1. **Gather.** `gh pr view`, `gh pr diff`, `gh pr checks`. Read `AGENTS.md`, `PROFILE.md`, and the nested `AGENTS.md`
   next to the changed files. Read changed files whole and their callers, not only hunks.
2. **Verify the classification** before the code. Wrong tier or missing trigger is itself a blocking finding.
3. **Verify evidence, do not recreate it (D1).** For each check the PR lists: does a CI result for the *head SHA* exist,
   did it run the tests that cover the changed paths, were any skipped/ignored/zero-test? Open the run log for the
   relevant job. Run something locally only to answer a specific question the logs cannot (say which question).
   If CI did not run because of a path filter, that is `skipped`, not `passed`; decide whether the filter was right.
4. **Correctness.** Edge cases (empty/zero/max/concurrent/failure paths), error handling, lifetimes/resources,
   wrong input assumptions, migrations' down path, time zones, retries duplicating side effects.
5. **Scope and hygiene.** Unrelated changes, debug leftovers, disabled checks, new suppressions without reason, secrets,
   hand-edited generated files, dependency additions (license, maintenance, size).
6. **Rules.** Anything the core R-rules, the overlay additions or Owner decisions forbid or reserve.
7. **Report.** Severities: blocking / should-fix / suggestion (suggestions never block). Each finding: file:line,
   concrete trigger, impact, confidence (high/medium/low), fix. Verdict: *ready* or *ready once blocking + should-fix
   are done*. On re-review list each earlier finding as resolved / not resolved / withdrawn.
8. **Bound the loop (R7).** Two cycles without agreement → write the disagreement in the PR and request owner decision.
