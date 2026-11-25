# Playbook: independent code review (every PR; core R4)

Reviewer input: PR number, the classification block, base/head SHAs. Not the author's conclusions.
Reviewer output: findings with severity, file:line, trigger, impact, confidence, fix; verdict; evidence used.

1. **Gather.** `gh pr view`, `gh pr diff`, `gh pr checks` (GitHub refuses a diff over 20,000 lines: then fetch the head and
   read `git diff <base>...<head>`). Read `AGENTS.md`, `PROFILE.md`, and the nested `AGENTS.md`
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
   are done*. On re-review list each earlier finding as resolved / not resolved / withdrawn, and number the round. Apply
   the stopping rule below before asking for another round. Check that the PR records the author's self-check
   (`PROFILE.md › Self-check`) for its head, and that it covers at most one stage or wave (`classification.md` step 7).
8. **Re-pin PRs (core D1, 0.1.27).** A PR `scripts/pin.py` generated is checked mechanically first: check out agent-rules at
   the tag the PR pins (`git -C ~/development/agent-rules worktree add <scratch dir> v<version>`, removed afterward, R10),
   check out the PR head, and run that checkout's `scripts/pin.py <PR head checkout>` (dry run). It must print
   `0 file(s) would change`; read its `note:` lines. Confirm the commit trailers. One evidence line. Every file in the diff
   that pin.py does not manage, and any difference the dry run reports, gets steps 4–6. `.agent-rules/core.md` and the
   playbooks were reviewed in agent-rules and are not read again; the other managed files are covered by the byte
   equality. A pin.py bug is outside this check. Compliance (D12) is `compliance-reviewer`'s, not this
   review's; this review checks only that a regulated path marks it.
9. **Bound the loop (R7).** Two cycles without agreement → write the disagreement in the PR and request owner decision.

## Stopping rule (core R4, since 0.1.30)
A round is one review verdict and the fix push that answers it. What may open the next one:
- **A suggestion never opens a round.** Take it in the same push as other fixes, or leave it.
- **Rounds 1 and 2:** a blocking finding or a should-fix.
- **From round 3 on:** only a blocking finding, or a should-fix that the latest diff itself introduced (a line that round's
  fix push added or changed). Any other should-fix (one that was there at the earlier round and was missed, or a new
  reading of unchanged text) is not fixed in another round: the author records it under the PR's `## Follow-ups` as
  `<finding> — <file:line> — <issue URL or open item>`, and the reviewer lists it as `follow-up` and may give **ready**
  over it. Never over an unrecorded one, and never over a blocking finding, whatever the round.
- A finding of a broken control under a bound standard is blocking (D12) and is never turned into a follow-up.
- Before asking for another round, run `.agent-rules/merge.sh --check <n>` (it merges and posts nothing): it shows which verdicts carry to the head
  and which do not, so only the reviews whose scope changed are asked to look again.
- A re-review records its revision as `ready @ <sha>` in the Verdict cell (or updates `@ <sha>` in the Reviewer cell),
  never only in the Re-review column, which `merge.sh` does not read as the reviewed revision.
- A verdict carried to a later head (core D5 › Carry) is not a new round; a reviewer asked to look again after a change in
  its scope reviews that change, and the round count goes on from where it was.
