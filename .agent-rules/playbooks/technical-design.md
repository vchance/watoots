# Playbook: technical design review (trigger: nontrivial behavior, API, algorithm, data model, migration)

When: before substantial implementation. A short design note in the PR or `docs/design/` is the artifact.
**Sketch first** (core §4, since 0.1.30): a change that adds a protocol, flow, contract or data model gets a one-page sketch
before any of it is built: the flows (who calls whom, in order), what each request and response carries, what is stored where
(and for how long), and the failure, cancel and timeout cases of each step. This review and the security review
(`security.md`) pass the sketch, with findings as below, before the build starts; the PR links the sketch and both verdicts.
A finding that would change the sketch is fixed in the sketch, not found later in code. The PR records the sketch's
verdicts as `- Sketch: <link>, security ready @ <sha>, technical-design ready @ <sha>`; the merge-gate rows of `## Reviews`
still review the build against it.
Evidence: contracts (types/schemas/CLI), alternatives considered, edge cases and invariants, verification plan
(which tests prove which invariant), compatibility and migration notes.
Review questions: Does the contract match callers? What breaks for existing data/clients? Which invariant is untested?
Is the migration reversible and tested both ways? Are failure modes enumerated? Is there a simpler design?
Findings: as code-review.md. A missing verification plan is should-fix; an untested destructive migration is blocking.
