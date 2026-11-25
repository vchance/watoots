<!-- .github/pull_request_template.md (proposed) -->
## What and why
{{one paragraph}}

## Acceptance criteria (core R11 › Requirements)
- [ ] {{observable outcome 1}}
- [ ] {{observable outcome 2}}

## Design (core R11 › Design) — note/ADR link or `n/a: {{reason}}`
{{link}}
<!-- A new protocol, flow, contract or data model (core §4, 0.1.30): link the one-page sketch (flows, what each request carries,
     what is stored where, failure / cancel / timeout cases) and the security and technical-design verdicts it passed before
     the build; `n/a` is not accepted for such a change. -->
- Sketch: {{<link>, security ready @ <sha>, technical-design ready @ <sha> / n/a: no new protocol, flow, contract or data model}} (the merge-gate rows below still review the build)
- Stage: {{the one stage or wave this PR is (classification.md step 7) / n/a}}

## Classification (core R2)
- Tier: {{low or standard or high or emergency — exactly one word}}
- Surfaces: {{components / platforms / user-visible?}}
- Triggered playbooks: code ✔ · ux ☐ · technical-design ☐ · architecture ☐ · security ☐ · performance ☐ · observability ☐ · compliance ☐
- Escalation triggers hit: none | {{which}}
- Self-check: run at {{sha7}} (`PROFILE.md › Self-check`) | n/a: the profile has no list yet

## Compliance (core D12) — only when compliance is marked; see `.agent-rules/playbooks/compliance.md`
- Standard: {{<repo>@<tag> (<sha>)}} · checklist: {{file}} · sections read: {{§…}}
- Surfaces: {{audit trail, signature, … | none}}
- Deviations: none | {{<item> — <section> — <reason> — <follow-up>}}

## Revisions
- base: `{{sha}}` ({{branch}}) · head: `{{sha}}` · integration revision tested: `{{sha, or name this repository's actual integration lane (scheduled run, push run, release lane) and when it covers this change; "none" if it has no lane}}`

## Evidence (per check: passed / failed / skipped / unavailable / n/a — with revision)
| Check | Result | Revision | Where |
|---|---|---|---|
| {{lint}} | passed | {{sha}} | CI run {{url}} |
| {{cargo test -p foo}} | passed (143 tests) | {{sha}} | CI run {{url}} |
| full suite | n/a (no escalation trigger) | — | — |
| {{CI job the change cannot affect}} | n/a: not selected (D2) | {{sha}} | CI run {{url}} (job skipped by the `changes` job) |
| {{device tests (D10, simulators)}} | {{passed (<device/OS>, n tests) / failed / manual (simulator, operator <owner|agent>, <date>, <sha7>, <device/OS>) / unavailable: no macOS runner / unavailable: no KVM on runner (<label>) / n/a: not selected (D2) / n/a: no mobile surface}} | {{sha}} | CI job / artifacts {{url}} |
| {{manual oracle}} | unavailable (needs build box) | — | owner to run before release |

## Reviews
| Review | Reviewer | Verdict | Findings (blocking/should-fix/suggestion) | Re-review of fixes |
|---|---|---|---|---|
| code | pr-reviewer @ {{sha}} | pending (the reviewer writes `ready`, or `ready once blocking and should-fix are done`) | 0 / 0 / 0 | — |
<!-- only when compliance is marked, add as a row: | compliance | compliance-reviewer @ <sha> | pending | 0 / 0 / 0 | — | -->
<!-- `@ <sha>` is the revision the reviewer read: a re-review writes `ready @ <sha>` in the Verdict cell (used first) or updates
     the Reviewer cell, never only the Re-review column (merge.sh does not read it as the reviewed revision). The label
     picks the scope (PROFILE.md › Review triggers › Carry scope); a label that is or lists `code` (`code and security`)
     has the whole diff as its scope, and a code-review row (`code` or `pr-reviewer`, alone or in a list) is required. `.agent-rules/merge.sh --check <n>` shows what carries. Leave it there when later commits do not touch the review's scope: merge.sh
     carries the verdict to the head and records "carried from <sha>" (core D5 › Carry); never rewrite it to a revision the
     reviewer did not read. -->

## Follow-ups (core R4 › stopping rule; a deviation from a bound standard goes under `## Compliance › Deviations` instead)
none | {{<finding> — <file:line> — <issue URL or open item>}}

## Limitations and not done
{{what was not tested, platforms not covered, assumptions}}

## Resources left (core R10)
none | {{what, where, owner, expiry}}

## Release and operate (core R11)
- Version/changelog: {{bumped / n/a}} · Deploy: {{how, when, by whom / n/a}} · Post-release watch (required for high and emergency): {{signal, duration, rollback trigger / n/a}}

## Rollback
{{revert commit / feature flag / migration down path}}
