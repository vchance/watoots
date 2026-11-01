# Playbook: performance review (trigger: hot path, query plan, storage format, algorithm, dependency, network call, rendering, startup, concurrency, resource limits)

Evidence: baseline and candidate measured on the same hardware (named in `PROFILE.md`), repeated ≥5 runs, variance
reported, workload described; latency p50/p95/p99, throughput, memory/CPU, binary size or startup time as relevant;
comparison against the profile budget. Single-run numbers are not evidence.
Process: measure baseline *before* the change (on base SHA) → implement → measure candidate → compare → record in PR.
Do not add a flaky single-run CI gate; benchmarks run in the scheduled or release lane unless the profile says otherwise.
Findings: a regression beyond budget is blocking; no baseline when the trigger fired is should-fix (owner may accept
with an exception entry).
