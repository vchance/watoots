# Playbook: compliance review (trigger: a regulated surface of a bound standard; core D12)

When: before editing (the author's self-check, so deviations are corrected or recorded up front) and before merge (the
independent `compliance-reviewer`, since core 0.1.27 a reviewer of its own, separate from `pr-reviewer`, recorded as the
`compliance` row of `## Reviews`). Input: the `compliance` row of `PROFILE.md › Review
triggers` (standard, pinned revision, checklist file, regulated paths) and the PR's `## Compliance` section.

1. **Read the pinned standard, not the latest.** `git -C <local checkout> show <sha>:<file>` or
   `gh api 'repos/<repo>/contents/<file>?ref=<sha>' -H 'Accept: application/vnd.github.raw'`. Read the checklist file first,
   then every section it maps to the surfaces touched. A pinned standard you cannot read is a **blocking** finding: say so and
   do not guess the rule.
2. **Name the surfaces.** From the diff, list which regulated surfaces change (for example audit trail, electronic signature,
   regulated records and their retention, access to regulated functions, time source, validation package, identity, exposure,
   configuration). The PR body's `- Surfaces:` line must match; a missing surface is a classification finding (R2).
3. **Blocking: a broken control.** Any of: an audit trail weakened, made optional or switchable by a person, or missing who /
   what / when / why for a regulated change; a signature that the standard's signature rule does not allow (unbound to the
   record, no meaning, wrong components, content shown differently from what was signed); a regulated record deleted,
   overwritten or made unreadable by an earlier or later release; access to a regulated function loosened; a contradiction
   of a recorded owner decision of the standard; a surface the standard reserves for its own review changed without that
   review; anything the checklist marks **block**. The checklist adds to this list, never subtracts.
4. **Should-fix: correct it or record it.** Any other possible deviation: validation or specification text lagging the
   behavior, regulated behavior with no requirement id or test trace, convention drift from the standard, a pin behind the
   standard's latest tag, a citation the standard marks unverified, anything the checklist marks **warn**. The author fixes
   it when it is in scope; otherwise it goes under `## Compliance › Deviations` as `<item> — <standard section> — <reason>
   — <follow-up: PR / issue / open item>`. The reviewer lists a recorded deviation it agrees is described truthfully as
   `recorded` (not an open should-fix) and gives **ready** over it, never over an unrecorded one; it says so when it thinks a
   recorded deviation is really blocking.
5. **Do not decide for the owner.** Whether a deviation is acceptable for good is the owner's (R8): propose an exception or
   an open item in the standard's repository, cite where the owner decided, and treat an uncited owner authority as blocking.
6. **Report** as code-review.md (D7), each finding naming the standard's section and the pinned revision you read.

PR body section (required when the classification ticks `compliance ✔`; `pr-evidence` checks the lines exist):

```
## Compliance (core D12)
- Standard: <repo>@<tag> (<sha>) · checklist: <file> · sections read: <§…>
- Surfaces: <audit trail, signature, …> | none
- Deviations: none | (list below)
  - <item> — <section> — <reason> — <follow-up>
```
