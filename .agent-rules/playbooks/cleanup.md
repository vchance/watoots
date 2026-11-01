# Playbook: cleanup (core R10; runs at the end of every session and after every CI job you own)

Principle: remove what you created once it has served its purpose; never remove what you did not create or cannot
prove is safe; report the rest. Removing someone else's work is worse than leaving yours behind.

## Before you end a session
1. **Processes.** Anything you started in the background (dev servers, watchers, `nohup` jobs, tunnels): stop it and
   confirm with `pgrep`. Record PIDs you could not stop.
2. **Worktrees.** `git worktree list`. For each `.claude/worktrees/*` you created: `git -C <wt> status --porcelain`
   must be empty and the branch merged into the default branch (`git branch --merged <default>`), then
   `git worktree remove <wt>`; finally `git worktree prune`. Locked worktrees, dirty worktrees and worktrees with
   unpushed commits are reported, not removed. Branches are kept.
3. **Scratch and temp.** Delete scratch clones, downloaded archives, screenshots already attached to the PR, and
   anything under your session's scratchpad. Nothing under `/tmp` that you did not create.
4. **Build outputs outside the repo.** Caches or targets you redirected (`CARGO_TARGET_DIR`, tmpfs seeds, `/data/build/<x>`)
   are removed or, if they are shared, their size is reported to the owner. In-repo `target/`, `build/`,
   `node_modules/` are normal and stay unless the profile says otherwise.
5. **Services and data.** Containers (`docker ps -a` filtered by the names you used), compose stacks (`down -v`),
   test databases and buckets, k3d/kind clusters, VMs you booted, simulators (`xcrun simctl shutdown`), emulators.
   Test data you seeded into a shared service is removed or named in the PR.
6. **Credentials.** Tokens, keys, WireGuard peers, service accounts or app passwords you minted for the task are
   revoked unless the PR says they must stay, with owner and expiry.
7. **Cloud and paid resources.** Ephemeral runners, clusters, buckets, instances: deleted, or listed under
   "Resources left" with cost per day, owner and expiry.
8. **Report.** PR body › "Resources left": `none`, or one line per item (what, where, owner, expiry).

## CI jobs you author or modify
- Each job tears down what it starts in the same job (`if: always()` step), including compose stacks, clusters and
  temporary buckets.
- Workspace hygiene on self-hosted runners: do not accumulate per-run directories; use `actions/checkout` `clean: true`
  or an explicit prune step; keep caches bounded (size cap or age-based prune) and keyed so a PR cannot poison main.
- Scheduled cleanup lane: a weekly job (or owner runbook) that reports and prunes runner caches, dangling images
  (`docker system prune` only with owner-approved filters), and stale build trees; its output is evidence, not noise.
- Never add a cleanup step that deletes outside the job's own workspace or caches without an owner decision in the profile.

## Repository-level hygiene (owner lane, agents report only)
- Stale branches (`worktree-*`, merged chore branches): list with last commit date; the owner deletes.
- Orphaned worktrees (`git worktree prune --dry-run`), large untracked files, leftover `.env`-like files.
- Self-hosted runner disks: sizes of `/data/build/*`, sccache, docker images — report with the date.

## What "safe to remove" means
Created by this session (you can show the command that created it) **and** no longer referenced (not in an open PR,
not a running job's workspace, not a locked worktree) **and** reproducible (a build, a clone, a cache) or already
preserved (merged, pushed, attached). If any part is uncertain, report instead.
