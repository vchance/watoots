# Playbook: architecture review (trigger: new boundary, service/storage choice, major dependency, distributed behavior, hard-to-reverse decision)

When: before committing to the approach; validate as built before merge.
Artifact: an ADR (repos with `docs/adr` or ADR-*.md already have the convention; others use the same format) stating
context, decision, alternatives, consequences, ownership, failure modes, compatibility, migration, rollback.
Evidence for merge: ADR merged or in the PR; the implementation references it; rollback path demonstrated or described.
Review questions: What new thing must now be operated? Who owns it? What happens when it is down/slow/wrong? How is it
undone? Does it duplicate something a sibling repo already provides (flag, don't edit)?
High tier: the ADR and the reviewer's verdict on it are the merge evidence (core D5); owner-reserved decisions (R8) are asked before the ADR is written.
