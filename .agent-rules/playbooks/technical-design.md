# Playbook: technical design review (trigger: nontrivial behavior, API, algorithm, data model, migration)

When: before substantial implementation. A short design note in the PR or `docs/design/` is the artifact.
Evidence: contracts (types/schemas/CLI), alternatives considered, edge cases and invariants, verification plan
(which tests prove which invariant), compatibility and migration notes.
Review questions: Does the contract match callers? What breaks for existing data/clients? Which invariant is untested?
Is the migration reversible and tested both ways? Are failure modes enumerated? Is there a simpler design?
Findings: as code-review.md. A missing verification plan is should-fix; an untested destructive migration is blocking.
