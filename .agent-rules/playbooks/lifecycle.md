# Playbook: lifecycle gates (core R11) — sound SDLC at every step

Why a separate playbook: the other playbooks say *how* to do each activity; this one says *when each is owed* and what
proves it happened, so no phase is skipped silently. Phases are proportionate to tier: a Low change passes through them
in minutes; a High change may take days. The sequence is the same.

| Phase | Low tier | Standard tier | High tier | NIST SSDF 1.1 practices (reference) |
|---|---|---|---|---|
| Requirements | one-line acceptance criterion in the PR | acceptance criteria as observable outcomes; owner-reserved decisions checked | criteria + threat/abuse cases; owner consulted before design | PO.1 (define requirements), PW.1 (design to meet requirements) |
| Design | n/a stated | short design note in PR for new contracts/data models | ADR; alternatives; failure modes; rollback; security and performance triggers answered | PW.1, PW.2 (design review), PO.5 (secure environments) |
| Build | focused commit | tests for changed behavior; docs/contracts updated | plus negative tests for denied paths; migrations with down path; no new suppressions | PW.4 (reuse well-secured components), PW.5 (secure coding), PW.6 (compiler/toolchain checks) |
| Verify | doc checks | impact-selected checks on head SHA; manual checks recorded | plus escalation triggers; full component suite; manual oracles with date/SHA/operator | PW.7 (code review), PW.8 (testing), PO.3 (toolchains) |
| Review | independent code review | + triggered specialist playbooks; findings dispositioned; bounded loops | + specialist playbooks' verdicts recorded; reviewer confirms tier and evidence | PW.7, RV.1 (identify vulnerabilities) |
| Release | n/a (docs) or merge only | integration lane green on the revision released (core §4; a skip run does not count); changelog if the profile keeps one | release lane: artifacts, provenance, release notes, deploy procedure, rollback rehearsed | PS.1-PS.3 (protect code, verify integrity, archive), PW.9 (secure defaults) |
| Operate | n/a | signals for the change verified; runbook touched if ops change | post-release watch for the profile's period; alert for the rollback signal | RV.1-RV.3 (identify, assess, analyze root causes) |
| Retire / maintain | — | dependency advisories on cadence | deprecations dated; stale branches/worktrees/resources removed via owner lane | PW.4, RV.2 (remediate) |

## Gate rules
1. **No silent skips.** Each phase appears in the PR body: evidence, or `n/a: <reason>`. The evidence lint checks the
   headings exist; the reviewer checks the reasons are honest.
2. **Order matters.** Design before substantial build (so the approach can still change cheaply); verify before
   review (so the reviewer reads evidence, not promises); release only from the integration-tested revision.
3. **Traceability.** Acceptance criteria → tests → PR → release notes. Repos with a requirements/traceability matrix
   (historian) update it in the same PR; others link the issue or task.
4. **Documentation is part of Build**, not a later phase: README, user guide, runbook, API contract and changelog change
   with the code that changes them.
5. **Emergency path.** Allowed phases to compress: Design → a paragraph; Verify → minimum safety checks named in the
   profile; Review → one reviewer; Release → immediate. Required: recorded declarer (owner or named human; see
   classification.md), rollback ready, and a dated follow-up PR that completes every deferred gate (tests, design note, full verification). The follow-up is High tier.
6. **Feedback loop.** Every post-release defect produces a test that would have caught it and a one-line note on which
   gate let it through (classification, selection map, review, release). Repeated misses change the profile, not the rule.

## What the reviewer checks for this playbook
- Acceptance criteria present and met (or n/a justified)? Design evidence proportionate to tier? Tests cover the changed
  behavior and the criteria? Verify evidence tied to the head SHA? Release/operate fields filled or n/a with a reason?
- Any phase marked n/a that the change clearly needed is a blocking finding (process skip), independent of code quality.
