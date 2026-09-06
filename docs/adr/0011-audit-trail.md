# ADR-0011 — An audit trail is a third hook, not a fourth name for the trace

Date: 2026-09-06. Status: accepted.

## Context

A survey of the landscape in September 2026 found this missing and cheap. ACT
prints every capability decision as it resolves, on by default; Wassette ships
grant and revoke tooling over per-component policy files. watoots decides more
about capabilities than either and keeps almost none of it.

It has three observers already, and the honest first question is whether a
fourth is redundant:

- **`GrantReport`** — every import classified, returned once at load. A
  snapshot, and only of load time.
- **`TraceHook`** — every crossing, as typed values, for record and replay. It
  answers *what happened*.
- **`PluginStats` / `PluginProfile`** — aggregates. Calls, fuel, peak memory,
  where the time went.

None of them answers *what was this plugin permitted to do, and what was it
refused*. And the runtime half of that question is currently invisible:

- a `wasi:logging` line dropped because it sat below the manifest's level
  ceiling leaves no record at all;
- a `limits.log_bytes`, `limits.transfer` or fuel ceiling being spent becomes
  an `Err` returned to one caller and is then gone;
- a reload refused for wanting an ungranted import is likewise only an error;
- and nothing anywhere records that a plugin was loaded, or from what bytes.

An incident is exactly when someone asks those questions, and by then the
answers have to already exist.

## Decision

**A fourth observer, `AuditHook`, carrying authorisation decisions only.**

It is separate from `TraceHook` rather than more variants on it, because the
two differ in every dimension that matters: the audience is a security reviewer
rather than someone debugging; the volume is decisions rather than crossings,
which is orders of magnitude smaller; the retention is an incident's lifetime
rather than a session's; and a trace carries argument *values*, which are the
plugin's data and often the user's — an audit line must be safe to keep when a
trace is not.

That last point is the strongest argument for the split. `.github/ISSUE_TEMPLATE`
already warns a reporter to read a trace before pasting it, because it contains
every value that crossed the boundary. An audit trail has to be the thing you
*can* keep, so **audit events carry names and verdicts, never argument values**.

### What is an event

Authorisation decisions, and only those:

- a plugin loaded, and refused, with the import that decided it;
- each import's verdict at load — granted, denied, host-provided, types-only;
- a reload allowed or refused, since it re-runs the grant check;
- a log line admitted or suppressed by the level ceiling;
- a ceiling spent — which limit, not what was being carried.

Not crossings. If someone wants those, they want `TraceHook`, and pointing them
at it is a better answer than making this the same thing twice.

### Off in the library, on in the CLI

A library that writes to stderr uninvited is badly behaved, so the hook is opt
in and the application chooses the destination — it already has a logging
system and ours would be worse. The CLI turns it on, because a security feature
nobody enables is theatre, and the CLI is where someone is inspecting a plugin
rather than embedding one.

`docs/SECURITY.md` has to say plainly that an embedding application gets no
audit trail unless it installs one. A reader who assumes otherwise assumes it
in the direction that hurts.

### Observed, not reported

Same property as `PluginStats` and for the same reason (ADR-0006): every event
originates on the host side of the boundary, at the point the decision is made.
A guest can cause an event and can neither suppress nor forge one.

## Consequences

- One more thing to keep correct when a capability is added. A new grant that
  does not emit is a silent hole, so the check belongs in review rather than in
  a comment.
- The C surface takes a callback with a rendered line plus the structured
  fields, because a C host wants to forward it and a C++ host wants to match on
  it.
- Suppressed log lines become visible for the first time. That is the point —
  "the plugin said nothing" and "the plugin was not allowed to say it" have
  been indistinguishable, and they mean opposite things.
