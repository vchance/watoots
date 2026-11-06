# Playbook: change classification (always runs; core R2)

Output: a classification block in the PR body (template in `../pr-evidence-template.md`). Takes ~2 minutes; do it
before editing and again whenever scope changes.

1. **What changes.** List touched components (from `PROFILE.md › Components`), platforms, contracts (APIs, schemas,
   file formats, CLI flags), generated outputs, and anything a person sees.
2. **Tier.** Pick the highest that applies:
   - *Low*: prose, comments, formatting, mechanical rename; no behavior/contract/policy/generated-output/deploy effect.
   - *Standard*: ordinary behavior change inside one or a few components.
   - *High*: any of — trust boundary / authn / authz; destructive or irreversible migration; identity or tenant isolation;
     public compatibility break; regulated behavior; new service, storage or major dependency; release controls or
     deployment identity; **any file under `AGENTS.md`, `PROFILE.md`, `.claude/`, `.github/`, reviewer definitions**;
     a change that reduces a safeguard (deleting a test, widening a permission, skipping a check).
   - *Emergency*: production is down or data is at risk and the owner (or a human `PROFILE.md` names) has declared it; the
     agent never declares one. The PR records the declarer and the dated follow-up PR (R11).
   Size and extension do not decide tier. A one-line Markdown edit that changes agent authority is High.
3. **Triggered playbooks.** Code review always. Then from `PROFILE.md › Review triggers`: ux, technical-design,
   architecture, security, performance, observability. Name them; the reviewer loads only those.
4. **Checks that will serve as evidence.** From `PROFILE.md › Check selection`, list exactly which commands/jobs cover
   the touched paths. If the mapping is uncertain, say so and escalate (full component suite). Mark everything else
   `n/a` up front so nobody later mistakes "not run" for "passed".
5. **Escalation triggers hit?** shared code · toolchain/build · broad refactor · uncertain impact · release candidate ·
   escaped regression. Any yes → full suite for the affected component(s); say which.
   A change confined to `.github/**` is none of these since core 0.1.22 (D2): it selects no product job, and the
   scheduled integration run first exercises it.
6. **Owner-reserved?** Check `AGENTS.md › Owner decisions`. If the change touches one, stop and ask before coding.
