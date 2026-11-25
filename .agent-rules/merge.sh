#!/bin/sh
# Merge wrapper (core D5, every tier since core 0.1.9). Refuses unless: no check on the head SHA failed, is pending or was canceled
# (skipped or neutral checks are accepted); pr-evidence, ci-result (when the head's workflows define it, D2) and policy-drift (when
# policy paths changed, R9) have RUN AND PASSED: since core 0.1.21 "skipped" never satisfies those three, because a job with no
# RUNS_ON placement for the person who triggered it is skipped (D9) and a PR must not merge on checks nobody ran. The PR body must
# state exactly one tier (R2), carry the recorded verdict in every row of "## Reviews" (R4; no owner acceptance step, owner
# decision 2026-11-01) and reference the head SHA. Since core 0.1.30 a row reviewed at an earlier revision X counts for the head H
# when this script proves the PR's own change in that review's scope is the same at X and at H (D5 › Carry); it records the carry
# as a PR comment ("carried from X", R5), never as a new review.
set -eu
# 0.1.30: `--check` is a dry run of the Reviews gate that merges and posts nothing; any other argument refuses, so a misplaced
# or misspelled `--check` never merges.
usage() { echo "usage: .agent-rules/merge.sh [--check] <pr-number>"; exit 2; }
check=""; n=""; [ $# -ge 1 ] && [ $# -le 2 ] || usage
for a in "$@"; do
  case "$a" in --check) [ -z "$check" ] || usage; check=1;; ""|*[!0-9]*) usage;; *) [ -z "$n" ] || usage; n=$a;; esac
done
[ -n "$n" ] || usage
# core R4, D5 (0.1.30): every Reviews row ready for the head, or carried to it (CARRY_PY in agent-rules scripts/pin.py). Sets $carry.
# The program and the body go to temporary files first: a here-document inside $( ) trips some shells' parsers, and the body
# is untrusted text that stays out of the environment. The program runs from the top of the work tree.
run_carry() {
  prog=$(mktemp "${TMPDIR:-/tmp}/agent-rules-carry.XXXXXX"); bodyf=$(mktemp "${TMPDIR:-/tmp}/agent-rules-body.XXXXXX"); trap 'rm -f "$prog" "$bodyf"' EXIT
  [ -n "$prog" ] && [ -n "$bodyf" ] || { carry="not mergeable: cannot create temporary files"; return 1; }
  cat >"$prog" <<'AGENT_RULES_CARRY' || { carry="not mergeable: cannot write the carry program"; rm -f "$prog" "$bodyf"; return 1; }
# Review rows and verdict carry-forward (core R4, D5; since 0.1.30). Run by merge.sh from the top of the work tree:
# argv = head SHA, base-branch tip SHA, a file holding the PR body. Prints one "carried from X to H" line per carried row (and a
# "note:" line for each scope widened to the whole diff); exits 1, saying why, when a row is not ready for the head and cannot
# be carried to it. Nothing here edits the repository or the PR.
import os, posixpath, re, subprocess, sys

# Config that changes what `git diff` prints is pinned, and config injected through the environment is dropped, so the
# comparison means the same in every clone.
ENV = {k: v for k, v in os.environ.items() if not k.startswith(("GIT_CONFIG_COUNT", "GIT_CONFIG_KEY_", "GIT_CONFIG_VALUE_"))
       and k not in ("GIT_CONFIG_PARAMETERS", "GIT_EXTERNAL_DIFF", "GIT_DIFF_OPTS")}
PIN = ["-c", "core.quotePath=false", "-c", "diff.noprefix=false", "-c", "diff.mnemonicPrefix=false", "-c", "diff.relative=false",
       "-c", "diff.ignoreSubmodules=none", "-c", "diff.algorithm=myers", "-c", "diff.indentHeuristic=true", "-c", "diff.interHunkContext=0",
       "-c", "diff.suppressBlankEmpty=false", "-c", "diff.context=3", "-c", "color.ui=false", "-c", "core.autocrlf=false"]

def git(*a, ok=(0,)):
    r = subprocess.run(["git", *PIN, *a], capture_output=True, env=ENV)
    if r.returncode not in ok:
        print(f"not mergeable: git {a[0]} failed ({r.stderr.decode(errors='replace').strip()[:200]})"); sys.exit(1)
    return r

def rev(x):
    r = git("rev-parse", "--verify", "--quiet", x + "^{commit}", ok=(0, 1, 128))
    return r.stdout.decode().strip() if r.returncode == 0 else None

def ancestor(a, b): return git("merge-base", "--is-ancestor", a, b, ok=(0, 1)).returncode == 0

def merge_base(a, b):
    r = git("merge-base", a, b, ok=(0, 1))
    if r.returncode: print(f"not mergeable: {b[:7]} shares no history with the base branch"); sys.exit(1)
    return r.stdout.decode().strip()

_trees, _patches, _profiles = {}, {}, {}
def tree(c):
    """{path: (mode, object id)} for every entry of commit c, recursively (a submodule is one gitlink entry)."""
    if c not in _trees:
        out = git("ls-tree", "-r", "-z", "--full-tree", c).stdout.decode(errors="surrogateescape")
        t = {}
        for e in out.split("\0"):
            if e:
                meta, p = e.split("\t", 1); mode, _, oid = meta.split(" ")
                t[p] = (mode, oid)
        _trees[c] = t
    return _trees[c]

def profile(c):
    if c not in _profiles:
        r = git("show", f"{c}:PROFILE.md", ok=(0, 128))
        _profiles[c] = r.stdout.decode(errors="replace") if r.returncode == 0 else ""
    return _profiles[c]

def section(text, name):
    m = re.search(rf"^## {re.escape(name)}\b[^\n]*\n(.*?)(?=^## |\Z)", text, re.M | re.S)
    return m.group(1) if m else ""

def generated(text):
    """PROFILE.md › Generated paths: the first backticked glob of each `- ` item."""
    return {m.group(1).strip() for m in re.finditer(r"^- `([^`]+)`", section(text, "Generated paths"), re.M)}

def norm(s):
    s = re.sub(r"\(.*?\)", "", s.replace("`", "").replace("*", "")).strip().lower()
    return s[:-len("-review")] if s.endswith("-review") else s

def parts(label):
    """A row label may name several reviews ("code and security", "code + ux", "code, security")."""
    return [norm(x) for x in re.split(r",|\+|&|\band\b", re.sub(r"\(.*?\)", "", label)) if norm(x)]

CODE = {"code", "code review", "pr-reviewer"}  # norm() turns `code-review` into `code`

def scope(text, name):
    """The globs in the `Carry scope` column (found by its header) of the PROFILE.md › Review triggers row for this review,
    or None (no such column, no row, or no glob there): then the review's scope is the whole diff."""
    col = None
    for line in section(text, "Review triggers").splitlines():
        if not line.startswith("|"): continue
        c = line.split("|")
        if col is None:
            hits = [i for i, x in enumerate(c) if x.strip().lower().startswith("carry scope")]
            if not hits: return None
            col = hits[0]; continue
        if len(c) > col + 1 and norm(c[1]) == norm(name):
            gl = [g.strip() for x in re.findall(r"`([^`]+)`", c[col]) for g in x.split(",") if g.strip()]
            return gl or None
    return None

def glob_re(p, self_dir=True):
    """`**/` = any directories (or none), `**` = anything, `*` and `?` = within one path segment; a trailing `/` = everything
    below; any other character is literal. Matched against the whole repository-relative path, so a bare name matches only
    at the root."""
    p, i, r = p.strip().lstrip("/"), 0, ""
    tail = ""
    if p.endswith("/**") and self_dir: p, tail = p[:-3], "(?:/.*)?"  # in a scope, `x/**` is x and everything below it
    while i < len(p):
        if p.startswith("**/", i): r += "(?:.*/)?"; i += 3
        elif p.startswith("**", i): r += ".*"; i += 2
        elif p[i] == "*": r += "[^/]*"; i += 1
        elif p[i] == "?": r += "[^/]"; i += 1
        else: r += re.escape(p[i]); i += 1
    return re.compile(r + (".*" if p.endswith("/") else "") + tail + r"\Z")

def names(a, b):
    out = git("diff", "--name-only", "-z", "--no-renames", "--ignore-submodules=none", a, b).stdout.decode(errors="surrogateescape")
    return {p for p in out.split("\0") if p}

def patch(a, b, p):
    """The PR's own change to one path, without what moves when the base moves under it: blob ids (`index` lines) and hunk
    line numbers are dropped; the changed lines, their three lines of context, modes and binary content are kept."""
    k = (a, b, p)
    if k not in _patches:
        t = git("diff", "--no-color", "--no-ext-diff", "--no-textconv", "--binary", "--full-index", "--no-renames",
                "--ignore-submodules=none", "-U3", a, b, "--", ":(top,literal)" + p).stdout
        _patches[k] = [b"@@" if l.startswith(b"@@") else l for l in t.split(b"\n") if not l.startswith(b"index ")]
    return _patches[k]

def same(mbX, X, mbH, H, p):
    """Is the PR's own change to p the same at X and at H? Where the base did not change p, the file itself must be
    identical (blob and mode) at X and H; only where the base changed it is the line-number-free patch compared."""
    if tree(mbX).get(p) == tree(mbH).get(p): return tree(X).get(p) == tree(H).get(p)
    return patch(mbX, X, p) == patch(mbH, H, p)

def link_targets(commits, match):
    """Repository paths that in-scope symlinks of these commits point to, followed through chains (a symlink inside a target
    adds its own target) until nothing new is added. A target outside the repository is dropped. Returns None when a target
    is the repository root: then the scope is the whole diff."""
    out, grew = set(), True
    # A symlink is followed when it is in scope, inside a target, or a leading directory of a target (a symlinked
    # directory a target path runs through: its own target, as a whole, joins the scope).
    inside = lambda p: match(p) or p in out or any(p.startswith(t + "/") or t.startswith(p + "/") for t in out)
    while grew:
        grew = False
        for c in commits:
            for p, (mode, oid) in tree(c).items():
                if mode != "120000" or not inside(p): continue
                tgt = git("cat-file", "blob", oid).stdout.decode(errors="surrogateescape")
                if tgt.startswith("/"): continue
                q = posixpath.normpath(posixpath.join(posixpath.dirname(p), tgt))
                if q == ".": return None
                if q.startswith("../") or q == ".." or q in out: continue
                out.add(q); grew = True
    return out

def main():
    head, base, body_file = sys.argv[1], sys.argv[2], sys.argv[3]
    with open(body_file, encoding="utf-8", errors="replace") as f: body = f.read().replace("\r", "")
    m = re.search(r"^## Reviews[^\n]*\n(.*?)(?=^## |\Z)", body, re.M | re.S)
    rows = [l for l in (m.group(1) if m else "").splitlines() if l.startswith("|")][2:]
    rows = [l for l in rows if len(l.split("|")) >= 5]
    if not rows: print("not mergeable: the PR body's '## Reviews' table has no review rows (core R4)"); sys.exit(1)
    H, B = rev(head), rev(base)
    if not H or not B: print("not mergeable: the head or the base branch tip is not in this clone"); sys.exit(1)
    mbH = merge_base(B, H)
    gen = generated(profile(B)) & generated(profile(mbH)) & generated(profile(H))  # the PR cannot declare its own files generated
    gen_re = [glob_re(g, self_dir=False) for g in sorted(gen)]  # a generated `x/**` is only what is below x
    def whole(name, revs):
        """Is this row's scope the whole diff? A code row (by label), a row naming several reviews, or a review with no
        `Carry scope` globs at one of these revisions."""
        ps = parts(name)
        return not ps or len(ps) > 1 or ps[0] in CODE or any(scope(profile(r), ps[0]) is None for r in revs)
    if not any(set(parts(l.split("|")[1])) & CODE for l in rows):
        print("not mergeable: no code-review row in the PR body's '## Reviews' table: label the code review's row `code` (or a"
              " list that includes it, such as `code and security`) (core R4, D5)"); sys.exit(1)
    AT = r"@\s*`?([0-9a-f]{7,40})(?![0-9a-z])"
    carried, notes, bad = [], [], []
    for row in rows:
        c = row.split("|"); name = c[1].strip(" \t`*").replace("`", "") or "?"; verdict = c[3].strip(" \t`*").lower()
        if not re.match(r"ready([^a-z]|$)", verdict) or verdict.startswith("ready once"):
            bad.append(f"`{name}`: the verdict is not 'ready'"); continue
        # The reviewed revision: the Verdict cell's `@ <sha>` (a re-review records it there), else the Reviewer cell's. A bare
        # mention of the head counts only in a row with no `@ <sha>` at all (as before 0.1.30).
        # Any `@ <sha>` in those two cells that is the head passes the row.
        if any(rev(s) == H for s in re.findall(AT, c[2]) + re.findall(AT, c[3])): continue
        at = re.search(AT, c[3]) or re.search(AT, c[2])
        if not at:
            if H[:7] not in row: bad.append(f"`{name}`: names neither its reviewed revision ('@ <sha>') nor the head {H[:7]}")
            continue
        X = rev(at.group(1))
        if not X: bad.append(f"`{name}`: reviewed revision {at.group(1)} is not a commit in this clone"); continue
        if X == H: continue
        if not ancestor(X, H): bad.append(f"`{name}` @ {X[:7]}: not carried, {X[:7]} is not an ancestor of the head {H[:7]}"); continue
        if ancestor(X, B): bad.append(f"`{name}` @ {X[:7]}: not carried, {X[:7]} is on the base branch, not a revision of this PR"); continue
        mbX = merge_base(B, X)
        sc, why = None, ""
        if not whole(name, ()):
            found = [scope(profile(r), parts(name)[0]) for r in (B, mbH, X, H)]
            if None in found: why = "no `Carry scope` globs for this review in PROFILE.md at the base, X or H"
            else:
                sc = sorted({g for f in found for g in f})
                odd = [g for g in sc if re.search(r"[{}\[\]]", g)]
                empty = [g for g in sc if g not in odd and not all(any(glob_re(g).match(p) for p in tree(r)) for r in (mbH, H))]
                if odd: why = "glob(s) " + ", ".join(f"`{g}`" for g in odd) + " use { } [ ] or a placeholder, which this matcher does not read"
                elif empty: why = "glob(s) " + ", ".join(f"`{g}`" for g in empty) + " match no path at the merge base or at the head"
                if why: sc = None
            if why: notes.append(f"note: `{name}`: scope is the whole diff: {why}")
        if sc is not None:
            res = [glob_re(g) for g in sc]
            base_match = lambda p, res=res: any(g.match(p) for g in res)
            tg = link_targets((X, H), base_match)
            if tg is None: sc = None; notes.append(f"note: `{name}`: scope is the whole diff: a symlink in scope points at the repository root")
        if sc is None: match = lambda p: True
        else: match = lambda p, bm=base_match, tg=tg: bm(p) or p in tg or any(p.startswith(t + "/") for t in tg)
        changed = names(mbX, X) | names(mbH, H)
        is_gen = lambda p: any(g.match(p) for g in gen_re)
        paths = sorted(p for p in changed if not is_gen(p) and match(p))
        moved = [p for p in paths if not same(mbX, X, mbH, H, p)]
        regen = sorted(p for p in changed if is_gen(p) and not same(mbX, X, mbH, H, p))
        desc = "the whole diff except generated paths" if sc is None else ", ".join(f"`{g}`" for g in sc) + " (generated paths excluded)"
        if moved:
            bad.append(f"`{name}` @ {X[:7]}: not carried to {H[:7]}, the PR's own change in its scope ({desc}) differs: "
                       + ", ".join(moved[:10]) + (" ..." if len(moved) > 10 else "")); continue
        carried.append(f"`{name}`: carried from {X[:7]} to {H[:7]}: no change in {desc}"
                       + ("; generated paths changed since X: " + ", ".join(regen[:10]) + (" ..." if len(regen) > 10 else "") if regen else ""))
    for l in carried + notes: print(l)
    if bad:
        for l in bad: print("not mergeable:", l)
        sys.exit(1)
    print(f"carry-check: {len(rows)} rows")  # merge.sh refuses without this line: a truncated or empty program proves nothing

if __name__ == "__main__":
    main()
AGENT_RULES_CARRY
  printf '%s\n' "$body" >"$bodyf" || { carry="not mergeable: cannot write the PR body"; rm -f "$prog" "$bodyf"; return 1; }
  rc=0; top=$(git rev-parse --show-toplevel) || { carry="not mergeable: cannot find the top of the work tree"; rm -f "$prog" "$bodyf"; return 1; }
  carry=$(cd "$top" && python3 -I "$prog" "$sha" "$base_sha" "$bodyf") || rc=$?
  rm -f "$prog" "$bodyf"
  [ "$rc" = 0 ] || return "$rc"
  # Fail closed: the program's last line counts the rows it checked; without it (an empty or cut-off program) nothing was proved.
  last=$(printf '%s\n' "$carry" | tail -n 1)
  case "$last" in "carry-check: 0 rows"|"carry-check: "*[!0-9]*" rows") carry="not mergeable: the carry check did not run (got: $last)"; return 1;; "carry-check: "[0-9]*" rows") ;;
    *) carry="not mergeable: the carry check did not finish (no 'carry-check:' line)"; return 1;; esac
  carry=$(printf '%s\n' "$carry" | sed '$d')
}
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
# core D5 (0.1.30): the base branch tip, for the merge bases a carried verdict is checked against.
base=$(gh pr view "$n" --json baseRefName -q .baseRefName); [ -n "$base" ] || { echo "cannot read the base branch of PR $n"; exit 1; }
GIT_TERMINAL_PROMPT=0 git -c credential.helper= -c credential.helper='!gh auth git-credential' fetch -q origin "refs/heads/$base" 2>/dev/null \
  || { echo "cannot fetch the base branch $base"; exit 1; }
base_sha=$(git rev-parse FETCH_HEAD)
if [ -n "$check" ]; then
  body=$(gh pr view "$n" --json body -q .body | tr -d '\r')
  if run_carry; then rc=0; else rc=1; fi
  [ -z "$carry" ] || printf '%s\n' "$carry"
  if [ "$rc" = 0 ]; then echo "merge.sh --check: every Reviews row is ready for the head $sha or carried to it; nothing merged or posted"
  else echo "merge.sh --check: refused for head $sha; nothing merged or posted"; fi
  exit "$rc"
fi
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
run_carry || { printf '%s\n' "$carry"; echo "every row of the PR body's '## Reviews' table must carry the verdict 'ready' for the head SHA $(printf '%s' "$sha" | cut -c1-7), or one merge.sh can carry to it (core R4, D5): finish the independent review first"; exit 1; }
[ -z "$carry" ] || printf '%s\n' "$carry"
printf '%s\n' "$body" | grep -q "$(printf '%s' "$sha" | cut -c1-7)" || { echo "PR body does not reference head $sha: update the evidence table"; exit 1; }
[ "$(gh pr view "$n" --json headRefOid -q .headRefOid)" = "$sha" ] || { echo "head moved while checking; re-run"; exit 1; }
[ "$(gh pr view "$n" --json baseRefName -q .baseRefName)" = "$base" ] || { echo "base branch changed while checking; re-run"; exit 1; }
# R5: a carried verdict is recorded on the PR as carried, not as a review of the head; no record, no merge.
case "$carry" in *"carried from"*)
  msg=$(printf 'merge.sh (core D5): review verdicts carried to head %s, not re-reviewed:\n\n%s' "$sha" "$carry")
  # A re-run posts no second copy, but only this account's own comment with exactly this text counts as the record: anyone
  # else's comment, however worded, is ignored.
  me=$(gh api user -q .login 2>/dev/null) || me=""
  found=""
  if printf '%s' "$me" | grep -qE '^[A-Za-z0-9-]+$'; then
    mj=$(printf '%s' "$msg" | python3 -I -c 'import json,sys; print(json.dumps(sys.stdin.read()))')
    found=$(gh api --paginate "repos/{owner}/{repo}/issues/$n/comments" -q ".[] | select(.user.login == \"$me\" and .body == $mj) | \"recorded\"" 2>/dev/null) || found=""
  fi
  if [ -n "$found" ]; then echo "the carried verdicts are already recorded on PR $n by $me"
  else gh pr comment "$n" --body "$msg" >/dev/null || { echo "cannot record the carried verdicts on PR $n"; exit 1; }; fi;;
esac
# --match-head-commit closes the window between the comparison above and the merge (the merge fails if the head moved).
exec gh pr merge "$n" --merge --match-head-commit "$sha"
