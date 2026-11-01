#!/bin/sh
# Merge wrapper (core D5). Refuses unless: every check on the head SHA succeeded (or was skipped/neutral), the
# pr-evidence check ran, the policy-drift check ran when policy paths changed, the PR body states a low or standard
# tier, no policy path changed (R9: those are high tier whatever the body says), and the body references the head SHA.
# High and emergency tiers are merged by the owner.
set -eu
n="${1:-}"; [ -n "$n" ] || { echo "usage: .agent-rules/merge.sh <pr-number>"; exit 2; }
sha=$(gh pr view "$n" --json headRefOid -q .headRefOid)
checks=$(gh pr checks "$n" --json name,state,bucket 2>/dev/null || gh pr checks "$n" --json name,state 2>/dev/null || echo '[]')
files=$(gh pr diff "$n" --name-only)
printf '%s' "$checks" | python3 -c '
import json,sys,re
cs=json.load(sys.stdin); files=sys.argv[1].split()
if not cs: print("not mergeable: the PR has no checks"); sys.exit(1)
ok=("pass","success","skipping","skipped","neutral")
bad=[c["name"] for c in cs if str(c.get("bucket") or c.get("state") or "").lower() not in ok]
names=" ".join(c["name"] for c in cs)
need=["pr-evidence"]
if any(re.match(r"(.*/)?(AGENTS\.md|CLAUDE\.md|GEMINI\.md|PROFILE\.md|\.claude/|\.codex/|\.cursor/|\.github/|\.agent-rules/|\.githooks/)", f) for f in files): need.append("policy-drift")
# policy-drift in need now always ends in the R9 refusal below; kept so the message names the check the owner must see green.
missing=[x for x in need if x not in names]
if "policy-drift" in need: print("policy paths changed (AGENTS/CLAUDE/GEMINI/PROFILE .md, .claude/ .codex/ .cursor/ .github/ .agent-rules/ .githooks/): high tier (core R9), owner must merge"); sys.exit(1)
if bad: print("not mergeable, checks not green:", bad); sys.exit(1)
if missing: print("not mergeable, required check(s) absent:", missing); sys.exit(1)' "$files"
body=$(gh pr view "$n" --json body -q .body | tr -d '\r')
printf '%s\n' "$body" | grep -qE '^- Tier: (low|standard)[[:space:]]*$' || { echo "tier is not exactly low or standard: owner must merge (core D5)"; exit 1; }
printf '%s\n' "$body" | grep -q "$(printf '%s' "$sha" | cut -c1-7)" || { echo "PR body does not reference head $sha: update the evidence table"; exit 1; }
[ "$(gh pr view "$n" --json headRefOid -q .headRefOid)" = "$sha" ] || { echo "head moved while checking; re-run"; exit 1; }
exec gh pr merge "$n" --merge
