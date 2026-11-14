<!-- .github/pull_request_template.md (proposed) -->
## What and why
{{one paragraph}}

## Acceptance criteria (core R11 › Requirements)
- [ ] {{observable outcome 1}}
- [ ] {{observable outcome 2}}

## Design (core R11 › Design) — note/ADR link or `n/a: {{reason}}`
{{link}}

## Classification (core R2)
- Tier: {{low or standard or high or emergency — exactly one word}}
- Surfaces: {{components / platforms / user-visible?}}
- Triggered playbooks: code ✔ · ux ☐ · technical-design ☐ · architecture ☐ · security ☐ · performance ☐ · observability ☐ · compliance ☐
- Escalation triggers hit: none | {{which}}

## Compliance (core D12) — only when compliance is ticked; see `.agent-rules/playbooks/compliance.md`
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
<!-- only when compliance is ticked, add as a row: | compliance | compliance-reviewer @ <sha> | pending | 0 / 0 / 0 | — | -->

## Limitations and not done
{{what was not tested, platforms not covered, assumptions}}

## Resources left (core R10)
none | {{what, where, owner, expiry}}

## Release and operate (core R11)
- Version/changelog: {{bumped / n/a}} · Deploy: {{how, when, by whom / n/a}} · Post-release watch (required for high and emergency): {{signal, duration, rollback trigger / n/a}}

## Rollback
{{revert commit / feature flag / migration down path}}
