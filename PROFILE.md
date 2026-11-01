# PROFILE.md — repository profile (template; a documented table, not YAML — automation can come later if it earns it)

| Field | Value |
|---|---|
| Repository | {{slug}} · default branch `{{main}}` · visibility {{private/public}} · GitHub plan {{free/pro/team}} (determines whether rulesets exist) |
| Classification | {{active product / library / infrastructure / prototype / archived / docs}} |
| Accountable owner | {{name}} — merge authority for **high** tier; approver of exceptions |
| Supported platforms | {{e.g. x86_64 Linux (CI), aarch64 Linux (manual), Windows (nobody), iOS 17+ (manual on owner Mac)}} — mark each `CI` / `manual` / `nobody` |
| Toolchain | {{rust 1.xx (rust-toolchain.toml) / node 22 / cmake 3.28 ...}} |
| Required services for tests | {{Postgres w/ CREATEDB, MinIO, none ...}} and how CI provides them |

## Components and critical paths
| Component (path glob) | Owner | Critical? | Tests that cover it (command) | Consumers (transitive) |
|---|---|---|---|---|
| `{{crates/foo/**}}` | {{owner}} | yes/no | `{{cargo test -p foo}}` | `{{bar, baz}}` |

## Check selection (impact-based; core D2)
| Changed paths | Checks that run on the PR | Evidence recorded as |
|---|---|---|
| `**/*.md` not under `docs/requirements/`, `AGENTS.md`/`PROFILE.md` excluded (R9) | doc checks (links, lint) | `docs: pass`; build/test `n/a` |
| `AGENTS.md`, `PROFILE.md`, `.claude/**`, `.github/**` | policy drift check + full CI of affected workflows | per check |
| `{{crates/foo/**}}` | `{{cargo test -p foo -p bar}}` + lint + format | per check |
| `Cargo.lock`, `rust-toolchain.toml`, `package-lock.json`, build scripts, CI yaml, shared crates `{{...}}` | **escalation: full suite** | per check |
| Uncertain mapping / `{{>N}}` files touched / cross-component refactor | **escalation: full suite** | per check |

Escalation triggers (any one → full component suite; `{{...}}` → whole repository): shared or widely-consumed code,
toolchain/build-system change, broad refactor, uncertain impact, release candidate, a regression that escaped selection.

## Lanes
| Lane | What runs | When | Owner of a failure |
|---|---|---|---|
| PR (fast) | selected checks above | every PR commit | author |
| Integration batch | `{{full suite on main after merge, or merge-group}}` | on merge to main (push) — **once per merge**, not per PR commit | merger |
| Scheduled | `{{audit, dependency advisories, platform matrix, soak}}` | `{{weekly Mon 06:00}}` | owner |
| Release | `{{full matrix + packaging + oracles}}` | tag / release PR | owner |
| Manual with evidence | `{{S7/BACnet oracles, iOS on owner Mac}}` | before release; result pasted in PR/release notes with date and revision | owner |

## Review triggers
| Playbook | Triggered by paths/changes |
|---|---|
| ux-review | `{{ui/**, styles/**, templates/**, rendered output fixtures}}` |
| technical-design | new public API, algorithm, data model, migration |
| architecture | new service/storage/major dependency, cross-repo boundary |
| security | `{{auth/**, api/**, uploads, deps, .github/**, permissions}}` |
| performance | `{{hot paths}}`; budgets: {{p95 latency, startup, binary size, memory}} measured by `{{bench cmd}}` on `{{hardware}}` |
| observability | runtime behavior or failure-mode changes in `{{services}}` |

## Merge authority
- Low/Standard: agent auto-merge once core R3–R6 hold (D5).
- High: `{{owner}}` comments `approved: <sha>` on the PR before merge. Emergency: `{{approver}}`, follow-up PR within `{{N}}` days.
- Server-side enforcement available: {{none — free plan private repo | rulesets: required checks X, Y}}. If none, the
  `gh pr merge` guard (agent-rules `templates/merge-guard.md`; in the repo: `.agent-rules/merge.sh`) is the only mechanism and this is a known gap.

## Exceptions (governed)
R-rule gaps only (core §1): what the repository cannot yet satisfy, recorded with authority and expiry. A deviation from a
D-rule is an override, not an exception: in force only as an `overrides: Dn` block in `AGENTS.md › Overrides` (core §5.2).
| Id | Rule | Replacement | Rationale | Authority | Scope | Expiry/review |
|---|---|---|---|---|---|---|
| X1 | {{R5}} | {{...}} | {{...}} | {{owner, date}} — or `proposed — owner decision` | {{paths}} | {{date}} |

### Proposed D-rule overrides (not in force)
Moved into `AGENTS.md › Overrides` only once the owner accepts it; until then the core default stands as written.
- proposed `overrides: {{Dn}}` — {{replacement}} — why: {{...}} — authority: owner decision pending — scope: {{paths}} — review: {{date}}

## Release and operations (core R11)
| Field | Value |
|---|---|
| Versioning | {{semver via Cargo.toml / tags / marketing version}} and who bumps it |
| Changelog / release notes | {{CHANGELOG.md kept per PR / generated from PR titles / none for internal tools}} |
| Release procedure | {{tag → release lane → artifacts → deploy command or store submission}}; owner-only steps marked |
| Deploy targets | {{synerlytics docker compose / App Store / crates.io / none}} |
| Rollback | {{revert + redeploy / previous tag / migration down path}}; tested how often |
| Post-release watch | {{signals to watch, for how long, who}} |
| Dependency cadence | {{weekly audit; Dependabot grouped; majors are owner decisions}} |
| Deprecation policy | {{how long a deprecated API/flag stays, how announced}} |

## Cleanup (core R10)
| Resource this repo's work creates | Where | Teardown command | Safe to remove when |
|---|---|---|---|
| Build output | `{{target/, build/, node_modules/, .build/}}` | `{{cargo clean -p <crate> / rm -rf build/<preset>}}` | always (rebuilds) |
| Test services | `{{docker compose -f docker-compose.test.yml}}` / `{{Postgres db per test run}}` | `{{docker compose ... down -v}}` | tests finished |
| Simulators / emulators | `{{xcrun simctl list booted}}` | `{{xcrun simctl shutdown <udid>}}` | capture done |
| Worktrees | `.claude/worktrees/*` | `git worktree remove <path> && git worktree prune` | branch merged, `git status` clean |
| CI runner artifacts | `{{/data/build/<repo>, sccache}}` | `{{owner runbook}}`; agents only report sizes | owner decision |
| Cloud / paid | `{{Ubicloud runner minutes, k3d clusters, buckets}}` | `{{k3d cluster delete <name>}}` | job finished |

## Budgets
- CI: `{{≤ N runner-minutes per PR commit; full suite ≤ M/week}}` — measured from `gh run list` job durations (no report tool yet; say `to measure` until there is a baseline).
- Performance: see Review triggers › performance.
