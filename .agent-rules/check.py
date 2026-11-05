#!/usr/bin/env python3
"""Policy drift / placeholder / section / D9 workflow lint (stdlib only, read-only).
Exit 1 with ::error:: lines for GitHub annotations. `workflow_errors(text)` is also loaded by pin.py (re-pin notes)."""
import argparse, hashlib, os, re, sys

# D9 runner classes (core 0.1.21): every `runs-on` is the class form, keyed by the triggering person, with NO hosted default,
# and its job carries the placement gate. Hygiene, not a control: a branch can carry any label and the job runs before this check
# fails the PR (core D9). Function names are case-insensitive in Actions. All regexes are linear (no nested quantifiers).
CLASSES = 'SMALL|MEDIUM|LARGE|ARM|MACOS|WINDOWS'
UNPLACED = '["unplaced"]'   # valid JSON, names no provider, matches no runner: a skipped job never evaluates to a hosted label
CLASS_RE = re.compile(r"""^\$\{\{\s*(?i:fromjson)\(\s*(?:(inputs\.[A-Za-z0-9_-]+)\s*\|\|\s*)?vars\[format\('RUNS_ON_(""" + CLASSES + r""")__(\{0\}|ID_\{0\})',\s*github\.(actor|triggering_actor|actor_id)\)\]\s*\|\|\s*'(\[[^']*\])'\s*\)\s*\}\}$""")
RUNS_ON_LINE = re.compile(r"""(?:^|[{,])[ \t]*["']?runs-on["']?[ \t]*:(.*)$""")  # linear; comment and spaces stripped in runs_on_value
JOB_RE = re.compile(r"^  ([A-Za-z_][A-Za-z0-9_-]*):[ \t]*(?:#.*)?$")
JOB_KEY_RUNS = re.compile(r"""^    ["']?runs-on["']?[ \t]*:""")
JOB_KEY_IF = re.compile(r"^    if:[ \t]*(.*)$")
JOB_USES = re.compile(r"""^    uses:[ \t]*["']?([^"'\s#]+)""")  # a job that calls a reusable workflow (no runs-on of its own)
PR_TRIGGER = re.compile(r'''^(?:on:.*|[ \t]+(?:-[ \t]+)?["']?)pull_request(?:_target)?\b''', re.M)  # also `  - pull_request` and a quoted key  # [ \t]+ not \s+: \s would match across blank lines (quadratic)
HOSTED_RE = re.compile(r'''^(?:\[[ \t]*)?["']?(?:ubuntu-latest|ubuntu-(?:22\.04|24\.04)(?:-arm)?|macos-(?:latest|13|14|15)|windows-(?:latest|2022|2025|11-arm))["']?(?:[ \t]*\])?$''')  # standard (free on public repositories) GitHub-hosted labels only, plain or a one-item list: no larger/paid runner names; extend when GitHub adds an image
PUBLIC_RE = re.compile(r'''^\|[ \t]*CI runners[^|\n]*\|[ \t]*[`*]*public: GitHub-hosted\b''', re.M | re.I)  # PROFILE marker: the row's value starts with it (owner decision 2026-11-05)
def public_hosted(profile_text): return bool(profile_text and PUBLIC_RE.search(profile_text))
POLICY_JOBS = ('pr-evidence', 'policy-drift')   # gated like any job; merge.sh requires them to have actually run, never "skipped"

def rd(p, errors=None):
    with open(p, encoding='utf-8', errors=errors) as h: return h.read()
def runs_on_value(m):
    """The value after `runs-on:`, with a trailing comment, surrounding quotes and a flow-mapping `}` (not an expression's `}}`) removed."""
    v = re.split(r'\s#', ' ' + m.group(1), maxsplit=1)[0].strip()  # drop a trailing comment
    while v.endswith('}') and not v.endswith('}}'): v = v[:-1].rstrip()
    v = v.strip('"\'').strip()
    if not v.startswith('$') and ',' in v: v = v.split(',')[0].strip()  # `{ runs-on: x, timeout-minutes: 5 }`
    return v
def gate_text(cls, idpart, key, inp):
    v = f"vars[format('RUNS_ON_{cls}__{idpart}', github.{key})]"
    return f"({inp} || {v}) != ''" if inp else f"{v} != ''"
def gate_ok(ifval, gate):
    """`if: ${{ <gate> }}` or `if: ${{ <gate> && <anything> }}`: the gate is the leftmost conjunct (one line, whitespace-normalized)."""
    n = re.sub(r'\s+', ' ', re.split(r'\s#', ' ' + ifval, maxsplit=1)[0].strip())
    pre = '${{ ' + gate
    if not n.startswith(pre): return False
    rest = n[len(pre):]
    return rest == ' }}' or (rest.startswith(' && (') and rest.endswith(') }}') and _balanced(rest[4:-3]))
def _balanced(s):
    """Linear scan: parentheses balance, never go below zero, and the first '(' closes only at the end (so `(a) || (b)` is not one group); quoted '…' skipped."""
    d, q = 0, False
    for i, ch in enumerate(s):
        if q: q = ch != "'"; continue
        if ch == "'": q = True
        elif ch == '(': d += 1
        elif ch == ')':
            d -= 1
            if d < 0 or (d == 0 and i != len(s) - 1): return False
    return d == 0 and not q
def ci_result_text(text):
    """Lines of the `ci-result` job (its header to the next job header)."""
    grab, buf, in_jobs = False, [], False
    for line in text.splitlines():
        if re.match(r'^jobs:[ \t]*(?:#.*)?$', line): in_jobs = True; continue
        if in_jobs and re.match(r'^\S', line) and not line.startswith('#'): in_jobs = False; grab = False
        jm = JOB_RE.match(line) if in_jobs else None
        if jm: grab = jm.group(1) == 'ci-result'
        if grab: buf.append(line)
    return '\n'.join(buf)
ARTIFACT_RE = re.compile(r'''^[ \t]*(?:-[ \t]*)?uses:[ \t]*["']?actions/(?:upload|download)-artifact[@/]''')  # [ \t] not \s, and no adjacent optional runs: linear
def input_errors(text, used):
    """Inputs that appear in a placement gate must not carry a `default` or be `type: choice` (a preselected label would be a hosted
    default hidden in the workflow). Linear scan of the first `inputs:` block(s): entries one level below, attributes deeper."""
    out, base, name, ind = [], None, None, 0
    for i, line in enumerate(text.splitlines(), 1):
        if re.match(r'^jobs:', line): break   # only the trigger-level `inputs:` (workflow_dispatch / workflow_call), never a step's `with:`
        if line.lstrip().startswith('#') or not line.strip(): continue
        ind = len(line) - len(line.lstrip(' '))
        if re.match(r'^\s+inputs:\s*$', line): base = ind; name = None; continue
        if base is None: continue
        if ind <= base: base = None; name = None; continue
        em = re.match(r'^\s+([A-Za-z0-9_-]+):\s*$', line)
        if em and ind == base + 2: name = em.group(1); continue
        if name in used:
            if re.match(r'^\s+type:\s*choice\b', line): out.append((i, f'input `{name}` is used in a placement gate and must not be `type: choice` (a preselected label is a hosted default in the workflow; D9)'))
            dm = re.match(r'^\s+default:\s*(.*?)\s*(?:#.*)?$', line)
            if dm and dm.group(1).strip('"\'') != '': out.append((i, f'input `{name}` is used in a placement gate and must have no non-empty `default` (D9)'))
    return out
STEP_START = re.compile(r'^([ \t]*)- (?:name|uses|run|id|if|env|with|shell|working-directory):')
CACHE_USE = re.compile(r"uses:[ \t]*[\"']?(?:actions/cache(?:/restore|/save)?|Swatinem/rust-cache)@|type=gha|^[ \t]+cache:[ \t]*(?![\"']?false\b)[A-Za-z0-9\"'$]|^[ \t]+(?:bundler-cache|enable-cache):[ \t]*(?:[\"']?(?:true|auto)\b|\$\{\{)", re.M)
# Setup actions that use GitHub's cache by default (no `cache:` line needed): they need the guard, or caching turned off explicitly.
CACHE_DEFAULT = re.compile(r"uses:[ \t]*[\"']?(?:actions/setup-go|astral-sh/setup-uv|gradle/actions/setup-gradle|jdx/mise-action)@")
SCCACHE_GHA = re.compile(r'''^[ \t]*(?:-[ \t]*)?(?:run:[ \t]*(?:[|>][-+]?[ \t]*)?)?(?:export[ \t]+)?SCCACHE_GHA_ENABLED[ \t]*[:=][ \t]*(?!["']?(?:false|off|0|no)\b)\S''', re.I)  # sccache's GitHub cache backend: any value but an explicit off (an expression counts as on)
CACHE_OFF = re.compile(r"^[ \t]+(?:cache:[ \t]*[\"']?false|enable-cache:[ \t]*[\"']?false|cache-disabled:[ \t]*[\"']?true)\b", re.M)
IF_LINE = re.compile(r'^[ \t]*(?:-[ \t]*)?if:(.*)$')
def guard_ok(body_lines, ind):
    """The step's own `if:` line (at the step's dash or one level in) carries both conjuncts: `runner.environment == 'self-hosted'`
    (a GitHub-hosted runner never uses GitHub's cache: fail closed) and `env.CI_LOCAL_CACHE == ''` (the box uses its local sibling)."""
    for l in body_lines:
        m = IF_LINE.match(l)
        if m and len(l) - len(l.lstrip(' ')) in (ind, ind + 2) or (m and l.lstrip().startswith('- ')):
            if guard_val_ok(m.group(1)): return True
    return False
GUARD = "runner.environment == 'self-hosted' && env.CI_LOCAL_CACHE == ''"
def guard_val_ok(v):
    """The step `if:` is exactly the D11 guard, or the guard `&& (<one balanced group>)`: an `||`, a negation or a guard buried in a longer
    expression could still run GitHub's cache on a hosted runner, so none of those count (whitespace-normalized, trailing comment dropped)."""
    n = re.sub(r'\s+', ' ', re.split(r'\s#', ' ' + v, maxsplit=1)[0].strip())
    if n.startswith('${{') and n.endswith('}}'): n = n[3:-2].strip()
    return n == GUARD or (n.startswith(GUARD + ' && (') and n.endswith(')') and _balanced(n[len(GUARD) + 4:]))
def step_errors(text):
    """D11 runtime guards, one linear pass. A step runs from its dash line until indentation returns to or below that dash; comment lines
    and trailing comments are dropped. A step that uses GitHub's cache service (actions/cache, rust-cache, setup `cache:`, Docker
    type=gha) needs the guard on its own `if:` line; a step touching the artifact store uses `$CI_ARTIFACT_STORE` behind `test -d`, never
    a literal path. The lint cannot see placement; review checks the rest against the PROFILE row."""
    out, blk = [], None
    def done(b):
        if not b: return
        i, ind, lines = b
        body = '\n'.join(lines)
        if (CACHE_USE.search(body) or (CACHE_DEFAULT.search(body) and not CACHE_OFF.search(body))) and not guard_ok(lines, ind):
            out.append((i, "step uses GitHub's cache service (a cache action, a setup `cache:`/`enable-cache:`/`bundler-cache:` input, Docker type=gha, or setup-go/setup-uv/setup-gradle, which cache by default unless turned off) without exactly the D11 runtime guard on its own `if:`: `${{ runner.environment == 'self-hosted' && env.CI_LOCAL_CACHE == '' }}`, optionally `&& (…)` (a hosted runner never uses GitHub's cache; a box runner declares CI_LOCAL_CACHE and uses its local sibling step; playbooks/testing-and-ci.md)"))
        if '/data/ci-artifacts' in body: out.append((i, 'step names the artifact store path literally: use `$CI_ARTIFACT_STORE` behind `test -n "$CI_ARTIFACT_STORE" && test -d "$CI_ARTIFACT_STORE"` and never `mkdir -p` the root (D11)'))
        elif 'CI_ARTIFACT_STORE' in body and not re.search(r'(?:test|\[)[ \t]+-d[ \t]+"?\$\{?CI_ARTIFACT_STORE', body): out.append((i, 'step writes to `$CI_ARTIFACT_STORE` without `test -d "$CI_ARTIFACT_STORE"` (D11): it must fail loudly on a runner with no store'))
    for i, line in enumerate(text.splitlines(), 1):
        st = line.lstrip(' ')
        if not st or st.startswith('#'): continue
        ind = len(line) - len(st)
        if blk and ind <= blk[1]: done(blk); blk = None
        if blk is None and STEP_START.match(line): blk = (i, ind, [])
        if blk: blk[2].append(re.split(r'\s#', ' ' + line, maxsplit=1)[0][1:])
    done(blk)
    return out + declaration_errors(text)
def declaration_errors(text):
    """Per job: a step `if:` that reads env.CI_LOCAL_CACHE must come after a `CI_LOCAL_CACHE=…GITHUB_ENV` line in the same job (the env
    context does not see variables inherited from the runner process, GitHub docs)."""
    out, job, declared, in_jobs, flagged = [], None, False, False, False
    for i, line in enumerate(text.splitlines(), 1):
        if line.lstrip().startswith('#'): continue
        if re.match(r'^jobs:[ \t]*(?:#.*)?$', line): in_jobs = True; continue
        if in_jobs and re.match(r'^\S', line): in_jobs = False
        jm = JOB_RE.match(line) if in_jobs else None
        if jm: job, declared, flagged = jm.group(1), False, False; continue
        if re.search(r'CI_LOCAL_CACHE=.*GITHUB_ENV', line): declared = True
        elif IF_LINE.match(line) and 'env.CI_LOCAL_CACHE' in line and not declared and not flagged:
            out.append((i, f'job `{job}` reads `env.CI_LOCAL_CACHE` before declaring it in this job (D11): add an earlier step `echo "CI_LOCAL_CACHE=${{CI_LOCAL_CACHE:-}}" >> "$GITHUB_ENV"`')); flagged = True
    return out
def workflow_errors(text, public=False):
    """Lint one workflow file: ([(line, message)], set of actor keys used). One-line `runs-on` and `if` in block-mapping jobs.
    public=True (the PROFILE `CI runners` row says `public: GitHub-hosted`, core D9): every job names one GitHub-hosted label and no
    placement variable, so fork pull requests and contributors with no variables still get CI."""
    out, keys, jobs, cur, in_jobs = [], set(), {}, None, False
    for i, line in enumerate(text.splitlines(), 1):
        if line.lstrip().startswith('#'): continue
        if re.match(r'^jobs:[ \t]*(?:#.*)?$', line): in_jobs = True; continue
        if in_jobs and re.match(r'^\S', line): in_jobs = False
        jm = JOB_RE.match(line) if in_jobs else None
        if jm: cur = jobs.setdefault(jm.group(1), {'if': None, 'runs': []}); continue
        if SCCACHE_GHA.match(line) or re.search(r'''SCCACHE_GHA_ENABLED=(?!["']?(?:false|off|0|no)\b).*GITHUB_ENV''', line, re.I): out.append((i, "`SCCACHE_GHA_ENABLED` turns on sccache's GitHub cache backend (D11): do not set it; point sccache at the box's local cache instead (`SCCACHE_DIR=$CI_LOCAL_CACHE/...` behind the D11 declaration step)"))
        if public and 'RUNS_ON_' in line.split(' #')[0]: out.append((i, 'a public GitHub-hosted repository carries no `RUNS_ON_*` placement (core D9): a gate on it would skip fork pull requests and contributors with no variables'))
        if ARTIFACT_RE.match(line): out.append((i, '`actions/upload-artifact` / `download-artifact` store on GitHub (D11, 0.1.21): write to the artifact store named in PROFILE instead'))
        im = JOB_KEY_IF.match(line) if (in_jobs and cur is not None) else None
        if im: cur['if'] = (i, im.group(1), line)
        um = JOB_USES.match(line) if (in_jobs and cur is not None) else None
        if um and not um.group(1).startswith('./'):
            out.append((i, f"job-level `uses: {um.group(1)}` calls a reusable workflow outside this repository: it runs on that file's own `runs-on`, with no placement gate this lint can check (D9). Call a local `./.github/workflows/<file>.yml` (linted like every workflow here) instead"))
        m = RUNS_ON_LINE.search(line)
        if not m: continue
        v = runs_on_value(m)
        if not v: out.append((i, 'multi-line `runs-on:` (block list or folded scalar); use the one-line D9 runner class expression')); continue
        if public:
            if not HOSTED_RE.match(v): out.append((i, f"`runs-on: {v}`: this public repository runs on GitHub-hosted runners (PROFILE `public: GitHub-hosted`, core D9): name one standard hosted label (ubuntu-latest, ubuntu-24.04, ubuntu-22.04, their -arm, macos-latest/13/14/15, windows-latest/2022/2025/11-arm), no placement variable, no self-hosted or larger (paid) runner"))
            continue
        c = CLASS_RE.match(v)
        if not c:
            out.append((i, f"`runs-on: {v}` is not a D9 runner class (expected one line in a block mapping: `${{{{ fromJSON(vars[format('RUNS_ON_<CLASS>__{{0}}', github.triggering_actor)] || '{UNPLACED}') }}}}`, optionally `inputs.<name> ||` first; no GitHub-hosted label anywhere in it: an explicit hosted placement lives only in the person's variable)")); continue
        inp, cls, idpart, key, fb = c.groups()
        if key == 'actor': out.append((i, "key `github.actor` stays the ORIGINAL actor on a re-run, so a placed person cannot place a bot's run by re-running it: use `github.triggering_actor` (D9, 0.1.21)"))
        if fb != UNPLACED: out.append((i, f"runner class {cls} must fall back to {UNPLACED}, not {fb}: no hosted default (D9, 0.1.21); a hosted placement is a person's variable such as [\"ubuntu-24.04\"]"))
        if (idpart == 'ID_{0}') != (key == 'actor_id'): out.append((i, 'key mismatch: `github.triggering_actor` pairs with `RUNS_ON_<CLASS>__{0}`, `github.actor_id` with `RUNS_ON_<CLASS>__ID_{0}`'))
        keys.add('actor_id' if key == 'actor_id' else 'triggering_actor')
        if cur is None or not JOB_KEY_RUNS.match(line): out.append((i, '`runs-on` must be a job-level key of a block-mapping job (4 spaces under `jobs:`) so its placement gate can be checked')); continue
        cur['runs'].append((i, cls, gate_text(cls, idpart, 'triggering_actor' if key == 'actor' else key, inp), inp))
    required, used = {}, set()
    for name, j in jobs.items():
        for (i, cls, gate, inp) in j['runs']:
            if inp: used.add(inp.split('.', 1)[1])
            if name not in POLICY_JOBS and name != 'ci-result' and not (j['if'] and re.search(r'#\s*d9-opt-in\b', j['if'][2])): required[name] = cls  # independent of gate validity: the first pass lists the whole ci-result requirement
            if j['if'] is None: out.append((i, f"job `{name}` has no job-level `if:`: add `if: ${{{{ {gate} }}}}` (or `${{{{ {gate} && <existing condition> }}}}`) so an unplaced person skips it instead of running somewhere else (D9, 0.1.21)"))
            elif not gate_ok(j['if'][1], gate): out.append((j['if'][0], f"job `{name}` `if:` must be one line starting with the placement gate `${{{{ {gate} }}}}` or `${{{{ {gate} && … }}}}`"))
    out += input_errors(text, used) + step_errors(text)
    if 'ci-result' in jobs:
        ev = None
        for i, line in enumerate(text.splitlines(), 1):
            if re.match(r'^jobs:', line): break
            em = re.match(r'^  ([a-z_]+):', line)
            if em: ev = em.group(1)
            if (ev == 'pull_request' and re.match(r'^    paths(?:-ignore)?:', line)) or re.match(r'^  pull_request:.*\bpaths(?:-ignore)?:', line):
                out.append((i, 'workflow-level `paths:`/`paths-ignore:` under `pull_request:` with a `ci-result` job: a PR the filter skips never provides `ci-result`, and merge.sh requires it. Replace the filter with a `changes` job that selects nothing for those paths (playbooks/testing-and-ci.md)')); break
    placed = [n for n, j in jobs.items() if j['runs'] and n not in POLICY_JOBS and n != 'ci-result']
    if placed and not required and 'ci-result' not in jobs and PR_TRIGGER.search(text):
        out.append((1, 'PR workflow places only opt-in job(s) ' + ', '.join(f'`{n}`' for n in sorted(placed)) + ' and defines no `ci-result` job: merge.sh refuses a head whose PR workflows place jobs without one (core D9). Add a trivial `ci-result` (placed on SMALL, `needs:` those jobs, `if: always()` behind the gate)'))
    if required and PR_TRIGGER.search(text):
        if 'ci-result' not in jobs: out.append((1, 'PR workflow gates job(s) ' + ', '.join(f'`{n}`' for n in sorted(required)) + ' on a placement but defines no `ci-result` job: an unplaced person would merge with those jobs skipped. Add the ci-result gate that fails on an unplaced required class (playbooks/testing-and-ci.md), or mark a job opt-in with a trailing `# d9-opt-in` comment on its `if:` line (skipped by design, covered another way, recorded in the PROFILE `CI runners` row)'))
        else:
            blk = ci_result_text(text); at = (jobs['ci-result']['runs'] or [(1,)])[0][0]
            for cls in sorted(set(required.values()) - {'SMALL'}):
                if f'RUNS_ON_{cls}__' not in blk: out.append((at, f'`ci-result` must fail when the {cls} placement is missing (a required class): read `RUNS_ON_{cls}__` for the triggering person in that job (playbooks/testing-and-ci.md)'))
    return out, keys

def main():
    ap = argparse.ArgumentParser(); ap.add_argument('--core', required=True); ap.add_argument('--agents', required=True); ap.add_argument('--profile'); ap.add_argument('--workflows', help='directory of GitHub workflows to lint for D9 runner classes')
    a = ap.parse_args(); errs = []; public = False
    core = rd(a.core); agents = rd(a.agents)
    digest = hashlib.sha256(core.encode()).hexdigest()[:12]
    if re.search(r'(?<!\$)\{\{\s*[A-Za-z0-9_]+\s*\}\}', core): errs.append('.agent-rules/core.md: unresolved placeholder in the materialized core (re-run pin.py)')
    m = re.search(r'core-digest:\s*([0-9a-f]{12})', agents)
    # NOTE: this is an integrity check (manifest digest == in-repo copy). Drift from the published core is detected by
    # re-running agent-rules/scripts/pin.py --check against the agent-rules checkout, not by this script.
    if not m: errs.append('AGENTS.md: missing core-digest in manifest line')
    elif m.group(1) != digest: errs.append(f'AGENTS.md: core-digest {m.group(1)} != pinned core {digest} (run agent-rules/scripts/pin.py)')
    for i, line in enumerate(agents.splitlines(), 1):
        if re.search(r'\{\{[A-Z_]+.*?\}\}|<!--\s*(One or two lines|Exact commands|Only the non-obvious|Generated files|Read by the design|Things the agent must ask)', line):
            errs.append(f'AGENTS.md:{i}: template placeholder still present')
    for rid in re.findall(r'\*\*(R\d+)\b', core):
        if re.search(rf'overrides:\s*{rid}\b', agents): errs.append(f'AGENTS.md: {rid} is non-overridable')
    if re.search(r'(push(es|ing)?|commit(s|ting)?) (directly |straight )?(to|on) `?(main|master)`?\b(?!.*(not|never|forbid))', agents, re.I):
        errs.append('AGENTS.md: text appears to permit pushing or committing to the default branch (contradicts R3); reword or remove')
    if a.profile and not os.path.exists(a.profile): errs.append(f'{a.profile}: missing (required by the manifest)')
    elif a.profile:
        prof = rd(a.profile); public = public_hosted(prof)
        if public and os.environ.get('REPO_PRIVATE') == 'true': errs.append('PROFILE.md: the `CI runners` row says `public: GitHub-hosted` but this repository is private (core D9): public mode is for public repositories only')
        for sec in ('Components', 'Check selection', 'Lanes', 'Review triggers', 'Merge authority', 'Release and operations', 'Cleanup'):
            if not re.search(rf'^## {re.escape(sec)}\b', prof, re.M): errs.append(f'PROFILE.md: missing section "## {sec}"')
        if re.search(r'(?<!\$)\{\{', prof): errs.append('PROFILE.md: template placeholders remain')  # `${{ … }}` expressions are allowed, like in the core
    for ag, pb in (('pr-reviewer', 'code-review'), ('design-ux-reviewer', 'ux-review')):
        f = os.path.join(os.path.dirname(a.agents) or '.', '.claude', 'agents', ag + '.md')
        if os.path.exists(f) and 'agent-rules adapter' in rd(f) and not os.path.exists(os.path.join(os.path.dirname(a.core), 'playbooks', pb + '.md')):
            errs.append(f'.agent-rules/playbooks/{pb}.md missing but .claude/agents/{ag}.md reads it (re-run agent-rules/scripts/pin.py)')
    if a.workflows and os.path.isdir(a.workflows):
        allkeys = set()
        for f in sorted(os.listdir(a.workflows)):
            fp = os.path.join(a.workflows, f)
            if f.endswith(('.yml', '.yaml')) and os.path.isfile(fp) and not os.path.islink(fp):
                out, keys = workflow_errors(rd(fp, errors='replace'), public=public); allkeys |= keys
                for i, msg in out: errs.append(f'{a.workflows}/{f}:{i}: {msg}')
        if len(allkeys) > 1: errs.append(f'{a.workflows}: workflows mix `github.triggering_actor` and `github.actor_id` keys; D9 makes that one repository-wide choice (PROFILE `CI runners` row)')
    for e in errs: print(f'::error::{e}')
    sys.exit(1 if errs else 0)

if __name__ == '__main__': main()
