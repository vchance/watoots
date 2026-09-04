# ADR-0006 — Logging is a granted capability; guest-emitted metrics are not built

Date: 2026-09-03. Status: accepted.

## Context

Default-deny has an edge nobody hits until they ship: **a plugin has no way to
say anything.** No filesystem, no sockets, no stdout worth the name — so a
plugin that wants to report "the config you gave me is malformed" has no
channel for it. The host learns that a call failed and nothing about why.

Every application adopting watoots therefore invents a logging interface. Ours
did: `examples/wit/lint.wit` declares `watoots:example/log` with an `emit`
function, and the C++ host implements it. That was fine as a demonstration of
host functions, but it is the wrong thing to be demonstrating. If every host
invents its own, no plugin is portable across two of them, and the single most
common host function in existence gets rewritten for each application.

The adjacent question — should the host offer metrics too — looks like the same
question and is not.

## Decision

### Logging: implement `wasi:logging`, as a manifest grant

Not `watoots:log`. [`wasi:logging`][wasi-logging] is one function —
`log(level, context, message)` — and adopting it means a plugin written for
wasmCloud or Spin logs correctly here with no changes. This follows the rule
already governing `wasm-tools`: wrap what exists, do not reimplement it.

It is a capability like any other, so it goes through the same machinery:

- A `Requirement::Logging` arm in `crates/host/src/imports.rs`, matched by the
  import-intersection check before instantiation.
- A manifest grant carrying a level ceiling — `logging = "warn"` — with absence
  meaning silence, consistent with everything else in `Permissions`.
- The **application supplies the sink** as a callback. watoots wires the
  interface and stays out of the logging-framework business. The C surface is
  `(level, const char*, const char*)`, which crosses the boundary without
  argument.

Two constraints fall out of the rest of the design and are not optional:

- **Timestamps come from the host, not the guest.** A guest-supplied timestamp
  would defeat the pinned wall clock and make a recording unreplayable.
- **Volume needs a ceiling.** Fuel bounds how long a plugin can loop; it does
  not bound the bytes each iteration pushes into your log pipeline. A per-call
  or per-plugin cap belongs in `Limits` alongside memory and fuel.

`wasi:logging` is [Phase 1][proposals] and can still move. That is an accepted
risk rather than an overlooked one: the surface is a single function, and if it
changes, the shim is smaller than the argument about whether to write it.

### Guest-emitted metrics: do not build them

Counters, gauges, histograms and labels reported *by the plugin* are out of
scope, and belong in `docs/SPEC.md`'s "what we deliberately don't build" list.

The blocking objection is a sandbox objection, which is why it belongs in this
project's threat model rather than its backlog: **label cardinality from
untrusted code is unbounded, and unbounded cardinality is a denial of service
on the metrics backend.** A plugin emitting `requests{id="…"}` with a fresh id
each iteration will kill Prometheus, and nothing in the sandbox stops it. Fuel
limits computation, not the cardinality of what crosses the boundary. We would
be handing an attacker a capability whose abuse lands on infrastructure we
cannot see and did not ship.

Three lesser reasons, any one of which would only be a delay:

- It is an aggregation and export pipeline, not a plugin-host feature.
- The histogram-and-label surface is genuinely unpleasant across a C ABI, and
  ADR-0003 commits us to designing every public API with that boundary in mind.
- [`wasi-otel`][wasi-otel] and [`wasi-observe`][wasi-observe] are early enough
  that adopting one now means betting on a moving target — and spans carry
  timing, which reintroduces exactly the nondeterminism the determinism knobs
  exist to remove.

If a standard settles and someone needs this, it arrives the same way logging
does: a manifest grant, default-deny, with the cardinality cap enforced by us
rather than hoped for.

### Host-observed metrics: already built, document it

The useful half of "metrics" needs no guest-facing API at all. `TraceHook` in
`crates/host/src/trace.rs` already observes every crossing, and the host
already computes fuel consumed, epoch time, memory high-water, denials, load
failures and cache hits.

Those numbers are **un-spoofable by the guest, bounded in cardinality, and
answer the question an operator actually asks** — which plugin is eating my
CPU — rather than the question a plugin can answer about itself. The work here
is documentation plus a `PluginStats` accessor over counters that exist, not a
subsystem.

## Consequences

- A plugin gains a portable way to report a problem, and it is recorded: a
  `wasi:logging` call is a host-import crossing, so a replayed bug report
  carries the plugin's own account of what it thought was happening. (See the
  addendum: "with no new machinery" was wrong.)
- `examples/wit/lint.wit` keeps `watoots:example/log`, which now demonstrates
  what it was always meant to — *an application's own* interface — rather than
  standing in for a missing standard one.
- `Permissions` grows a field and `Requirement` an arm; both are additive, and
  absence still denies. `Limits` grows a log-volume ceiling.
- We take a dependency on a Phase 1 proposal's shape. Budget for one breaking
  adjustment before it stabilises.
- Anyone wanting guest-emitted metrics is told no, in writing, with a reason
  they can evaluate. That is the point of the "don't build" list.

[wasi-logging]: https://github.com/WebAssembly/wasi-logging
[wasi-otel]: https://github.com/WebAssembly/wasi-otel
[wasi-observe]: https://github.com/WebAssembly/wasi-observe
[proposals]: https://github.com/WebAssembly/WASI/blob/main/Proposals.md

## Addendum — 2026-09-03, on implementation

Two corrections from wiring this up. Left here rather than edited into the text
above, because what a decision got wrong is worth more than a tidy record of it.

**"Record/replay captures it with no new machinery" was wrong.** Replay's mock
host serves the *application's* interfaces and deliberately ignores `wasi:`
ones, which the host library answers from the manifest in the trace header. So
a plugin that both logs and calls an application host function stalls the
replay cursor on the logging events and reports the next real import as a
divergence. Fixed by `Cursor::skip_unserved` in `crates/trace/src/replay.rs`,
with a regression test that fails without it. Small, but it is new machinery,
and the ADR should not have assumed otherwise from the outside.

**The shim must be installed on the `Linker` directly, not through
`HostBuilder::host_func`.** `imports::classify` consults `host_provided` before
it looks at the `wasi` namespace, so registering logging as an ordinary host
function would let an application shadow the capability check and make
`permissions.logging` decorative. This ADR said "the application supplies the
sink" and did not distinguish supplying a sink from supplying the interface.
Only the sink is the application's.
