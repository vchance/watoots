#!/usr/bin/env python3
"""Policy drift / placeholder / section check (proposal; stdlib only, read-only).
Exit 1 with ::error:: lines for GitHub annotations."""
import argparse, hashlib, re, sys
ap = argparse.ArgumentParser(); ap.add_argument('--core', required=True); ap.add_argument('--agents', required=True); ap.add_argument('--profile')
a = ap.parse_args(); errs = []
core = open(a.core, encoding='utf-8').read(); agents = open(a.agents, encoding='utf-8').read()
digest = hashlib.sha256(core.encode()).hexdigest()[:12]
if '{{' in core: errs.append('.agent-rules/core.md: unresolved placeholder in the materialized core (re-run pin.py)')
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
import os
if a.profile and not os.path.exists(a.profile): errs.append(f'{a.profile}: missing (required by the manifest)')
elif a.profile:
    prof = open(a.profile, encoding='utf-8').read()
    for sec in ('Components', 'Check selection', 'Lanes', 'Review triggers', 'Merge authority', 'Release and operations', 'Cleanup'):
        if not re.search(rf'^## {re.escape(sec)}\b', prof, re.M): errs.append(f'PROFILE.md: missing section "## {sec}"')
    if '{{' in prof: errs.append('PROFILE.md: template placeholders remain')
for e in errs: print(f'::error::{e}')
sys.exit(1 if errs else 0)
