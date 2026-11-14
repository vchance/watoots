#!/bin/sh
# Merge wrapper (core D5, every tier since core 0.1.9). Refuses unless: no check on the head SHA failed, is pending or was canceled
# (skipped or neutral checks are accepted); pr-evidence, ci-result (when the head's workflows define it, D2) and policy-drift (when
# policy paths changed, R9) have RUN AND PASSED: since core 0.1.21 "skipped" never satisfies those three, because a job with no
# RUNS_ON placement for the person who triggered it is skipped (D9) and a PR must not merge on checks nobody ran. The PR body must
# state exactly one tier (R2), carry the recorded verdict in every row of "## Reviews" (R4; no owner acceptance step, owner
# decision 2026-11-01) and reference the head SHA.
set -eu
n="${1:-}"; [ -n "$n" ] || { echo "usage: .agent-rules/merge.sh <pr-number>"; exit 2; }
sha=$(gh pr view "$n" --json headRefOid -q .headRefOid); [ -n "$sha" ] || { echo "cannot read the head SHA of PR $n"; exit 1; }
# gh exits non-zero while a check is pending or failing but still prints the JSON: keep one document, never two (no `a || b` concatenation).
checks=$(gh pr checks "$n" --json name,state,bucket 2>/dev/null) || true
[ -n "$checks" ] || checks=$(gh pr checks "$n" --json name,state 2>/dev/null) || true
[ -n "$checks" ] || checks='[]'
# The changed paths come from the pull-request files API, not `gh pr diff --name-only`: GitHub refuses a diff over 20,000 lines
# (HTTP 406, "diff exceeded the maximum number of lines"), which made every large PR unmergeable (0.1.25). A rename lists its old
# path too, so moving a file out of a policy path still requires policy-drift. The API lists at most 3,000 files: refuse a
# short list rather than judge the PR on part of it.
listing=$(gh api --paginate "repos/{owner}/{repo}/pulls/$n/files" -q '.[] | [.filename, (.previous_filename // "")] | @tsv')
listed=$(printf '%s\n' "$listing" | grep -c . || true); want=$(gh pr view "$n" --json changedFiles -q .changedFiles)
[ "$listed" = "$want" ] || { echo "not mergeable: the files API listed $listed of the $want changed files of PR $n (it stops at 3,000)"; exit 1; }
files=$(printf '%s\n' "$listing" | tr '\t' '\n' | grep . || true)
# core D2 (0.1.17): when the head's workflows define the ci-result gate job, its check is required too.
# Fetch through gh's credential helper with prompts off: a keychain helper can hang a non-interactive shell.
GIT_TERMINAL_PROMPT=0 git -c credential.helper= -c credential.helper='!gh auth git-credential' fetch -q origin "$sha" 2>/dev/null \
  || { echo "cannot fetch head $sha to read its workflows"; exit 1; }
gate=$(git grep -lE '^[[:space:]]+ci-result:' "$sha" -- ':/.github/workflows' 2>/dev/null | head -n 1 || true)
# core D9 (0.1.21): a pull_request workflow that places jobs (RUNS_ON_<CLASS>__) must define ci-result; without it, skipped product
# jobs would pass. Policy workflows and non-PR workflows (schedule, deploy) are not counted. An opt-in-only PR workflow adds a trivial ci-result.
gated=$(git grep -lE 'RUNS_ON_[A-Z]+__' "$sha" -- ':/.github/workflows' 2>/dev/null | grep -vE '/(policy-drift|pr-evidence)\.ya?ml$' \
  | while IFS= read -r f; do git show "$f" 2>/dev/null | grep -qE "^(on:.*|[[:space:]]+(-[[:space:]]+)?[\"']?)pull_request([^_a-z]|_target|\$)" && printf '%s\n' "${f#*:}"; done | head -n 1 || true)
[ -n "$gate" ] || [ -z "$gated" ] || { echo "not mergeable: $gated places jobs (RUNS_ON_<CLASS>__) on pull requests but no workflow defines a ci-result job, so skipped jobs would pass (core D9): add the ci-result gate (playbooks/testing-and-ci.md)"; exit 1; }
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
def bucket(c): return str(c.get("bucket") or c.get("state") or "").lower()
notrun=[x for x in need if x in names and any(bucket(c) not in ("pass","success") for c in cs if c["name"]==x)]  # every entry of that name must have run and passed
if bad: print("not mergeable, checks not green:", bad); sys.exit(1)
if missing: print("not mergeable, required check(s) absent:", missing); sys.exit(1)
if notrun: print("not mergeable, required check(s) did not run and pass (skipped = no RUNS_ON_<CLASS>__<login> placement for the person who triggered them; a placed person re-runs them, or edits/reopens the PR, core D9):", notrun); sys.exit(1)' "$files" "$gate"
body=$(gh pr view "$n" --json body -q .body | tr -d '\r')
[ "$(printf '%s\n' "$body" | grep -cE '^- Tier: (low|standard|high|emergency)[[:space:]]*$')" = 1 ] || { echo "PR body must state exactly one tier (core R2)"; exit 1; }
printf '%s\n' "$body" | awk '/^## Reviews/{f=1;next} f&&/^## /{exit} f&&/^\|/' | awk -F'|' -v sha="$(printf '%s' "$sha" | cut -c1-7)" 'NR>2 && NF>=5 {n++; v=$4; gsub(/^[ \t`*]+|[ \t`*]+$/,"",v); v=tolower(v); if (v ~ /^ready([^a-z]|$)/ && v !~ /^ready once/ && index($0, sha)) ok++} END{exit (n>0 && ok==n)?0:1}' \
  || { echo "every row of the PR body's '## Reviews' table must carry the verdict 'ready' and the head SHA $(printf '%s' "$sha" | cut -c1-7) (core R4, D5): finish the independent review first"; exit 1; }
printf '%s\n' "$body" | grep -q "$(printf '%s' "$sha" | cut -c1-7)" || { echo "PR body does not reference head $sha: update the evidence table"; exit 1; }
[ "$(gh pr view "$n" --json headRefOid -q .headRefOid)" = "$sha" ] || { echo "head moved while checking; re-run"; exit 1; }
# --match-head-commit closes the window between the comparison above and the merge (the merge fails if the head moved).
exec gh pr merge "$n" --merge --match-head-commit "$sha"
