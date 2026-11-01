# Playbook: observability review (trigger: changed runtime behavior or failure mode in a service/daemon)

Scope by project type: a service needs structured logs, metrics, traces/correlation ids, alerts, dashboards, runbook and
health/rollback signals; a CLI or library needs diagnostics (error messages with next step, `--verbose`, exit codes),
not a monitoring stack. `PROFILE.md` says which.
Evidence: what new signals exist and their cardinality/privacy check; a demonstrated failure path emitting the expected
signal (test or captured output); runbook change if operations change; success is not inferred from a log line alone.
Sources: OpenTelemetry semantic conventions; SRE "four golden signals" (see `../../evidence/sources.md`).
Findings: a new failure mode with no signal is should-fix; a change that silences an existing alert is blocking.
