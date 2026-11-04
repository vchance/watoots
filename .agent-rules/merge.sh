#!/bin/sh
# Merge wrapper (core D5, every tier since core 0.1.9). Refuses unless: every check on the head SHA succeeded (or was
# skipped/neutral), the pr-evidence check ran, the ci-result gate ran when the head's workflows define it (D2), the policy-drift check ran and passed when policy paths changed (R9), the
# PR body states exactly one tier, the body's "## Reviews" table carries a recorded verdict (R4; no owner acceptance step,
# owner decision 2026-11-01), and the body references the head SHA.
set -eu
n="${1:-}"; [ -n "$n" ] || { echo "usage: .agent-rules/merge.sh <pr-number>"; exit 2; }
sha=$(gh pr view "$n" --json headRefOid -q .headRefOid); [ -n "$sha" ] || { echo "cannot read the head SHA of PR $n"; exit 1; }
checks=$(gh pr checks "$n" --json name,state,bucket 2>/dev/null || gh pr checks "$n" --json name,state 2>/dev/null || echo '[]')
files=$(gh pr diff "$n" --name-only)
# core D2 (0.1.17): when the head's workflows define the ci-result gate job, its check is required too.
# Fetch through gh's credential helper with prompts off: a keychain helper can hang a non-interactive shell.
GIT_TERMINAL_PROMPT=0 git -c credential.helper= -c credential.helper='!gh auth git-credential' fetch -q origin "$sha" 2>/dev/null \
  || { echo "cannot fetch head $sha to read its workflows"; exit 1; }
gate=$(git grep -lE '^[[:space:]]+ci-result:' "$sha" -- ':/.github/workflows' 2>/dev/null | head -n 1 || true)
printf '%s' "$checks" | python3 -c '
import json,sys,re
cs=json.load(sys.stdin); files=sys.argv[1].split()
if not cs: print("not mergeable: the PR has no checks"); sys.exit(1)
ok=("pass","success","skipping","skipped","neutral")
bad=[c["name"] for c in cs if str(c.get("bucket") or c.get("state") or "").lower() not in ok]
names=[c["name"] for c in cs]  # exact names: a check merely containing "pr-evidence" must not satisfy the requirement
need=["pr-evidence"]
if sys.argv[2]: need.append("ci-result")
if any(re.match(r"(.*/)?(AGENTS\.md|CLAUDE\.md|GEMINI\.md|PROFILE\.md|\.claude/|\.codex/|\.cursor/|\.github/|\.agent-rules/|\.githooks/)", f) for f in files): need.append("policy-drift")
missing=[x for x in need if x not in names]
if bad: print("not mergeable, checks not green:", bad); sys.exit(1)
if missing: print("not mergeable, required check(s) absent:", missing); sys.exit(1)' "$files" "$gate"
body=$(gh pr view "$n" --json body -q .body | tr -d '\r')
[ "$(printf '%s\n' "$body" | grep -cE '^- Tier: (low|standard|high|emergency)[[:space:]]*$')" = 1 ] || { echo "PR body must state exactly one tier (core R2)"; exit 1; }
printf '%s\n' "$body" | awk '/^## Reviews/{f=1;next} f&&/^## /{exit} f&&/^\|/' | awk -F'|' -v sha="$(printf '%s' "$sha" | cut -c1-7)" 'NR>2 && NF>=5 {n++; v=$4; gsub(/^[ \t`*]+|[ \t`*]+$/,"",v); v=tolower(v); if (v ~ /^ready([^a-z]|$)/ && v !~ /^ready once/ && index($0, sha)) ok++} END{exit (n>0 && ok==n)?0:1}' \
  || { echo "every row of the PR body's '## Reviews' table must carry the verdict 'ready' and the head SHA $(printf '%s' "$sha" | cut -c1-7) (core R4, D5): finish the independent review first"; exit 1; }
printf '%s\n' "$body" | grep -q "$(printf '%s' "$sha" | cut -c1-7)" || { echo "PR body does not reference head $sha: update the evidence table"; exit 1; }
[ "$(gh pr view "$n" --json headRefOid -q .headRefOid)" = "$sha" ] || { echo "head moved while checking; re-run"; exit 1; }
exec gh pr merge "$n" --merge
