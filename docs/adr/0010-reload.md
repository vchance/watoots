# ADR-0010 — Reload carries state as WIT values, and checkpoint is not possible

Date: 2026-09-06. Status: accepted.

## Context

`docs/SPEC.md` lists as the last v0.2 candidate:

> Reload: drop/reinstantiate with optional `export-state`/`import-state` WIT
> hooks; experiment with same-binary checkpoint by copying linear memory,
> globals, and tables through the public API.

That is two proposals. The first is a mechanism; the second is an experiment
whose result decides whether the first is a compromise or the answer.

The spec's value table rates "same-binary checkpoint/restore" as **high — #4002
unsolved since 2022**, so the experiment is the interesting half and it is worth
running before designing around either outcome.

## Decision

### The experiment is over: checkpoint cannot be built on Wasmtime 48

A component's linear memory, globals and tables are **not reachable through the
public API**, so there is nothing to copy. Three independent checks:

- `ComponentItem` — what `Instance::get_export` can return — has exactly seven
  variants: `ComponentFunc`, `CoreFunc`, `Module`, `Component`,
  `ComponentInstance`, `Type`, `Resource`. No `Memory`, no `Global`, no `Table`.
- No public function anywhere under `runtime/component/` returns a
  `wasmtime::Memory`.
- `Instance::get_module` hands back a `Module`, which is the compiled artifact,
  not a live core instance holding state.

This is by design rather than an oversight: the canonical ABI treats a
component's memory as an implementation detail, and a component that exported it
would be handing callers the ability to corrupt its own invariants. It is the
same reasoning that stops watoots reading guest memory in a trace.

So the honest answer is that the wasmtime issue is unsolved *for us* too, and it
goes on `docs/SPEC.md`'s "what we deliberately don't build" list rather than
staying a backlog item that looks tractable.

### State crosses as typed WIT values, and that is better anyway

Reload is drop-and-reinstantiate. Where a plugin needs to survive it, the world
declares two optional exports — one that hands its state out, one that takes it
back — and the host carries the value between the old instance and the new.

The forced design turns out to be the right one, and it is worth saying why
rather than presenting it as making do:

- **A typed value can be recorded.** State crossing the boundary as WIT values
  goes through the same seam `TraceHook` watches, so a reload appears in a trace
  and replays. A memory image would be exactly what this project refuses to
  record — engine state, unreadable, unversioned.
- **A typed value survives a rebuild.** The point of reload is usually a *new*
  build of the plugin. A memory image is bound to one exact binary: the same
  source recompiled has a different layout and restoring into it is undefined.
  A WIT value is versioned by the world and survives anything the world's own
  compatibility rules allow.
- **A typed value can be refused.** The host sees it, so it can validate it,
  cap its size against `limits.transfer`, or decline it. An image is opaque.

### Reload re-runs the grant check, always

A reload takes new bytes, and new bytes may import more than the old ones did.
Reinstantiating without re-running the import-intersection check would let a
plugin acquire a capability by being updated, which is the sandbox failing at
precisely the moment it matters most — the moment the code changes.

So reload is `Host::load` plus a state handoff, not a cheaper path that skips
the checks. Same manifest, same intersection, same refusal on an ungranted
import. A reload that fails leaves the *old* instance running: a plugin that
cannot be replaced is better than a host left with none.

### What it is not

Not hot code loading. Not cross-version state migration — the spec already put
that on the "don't build" list, and nothing here changes it. The state a plugin
hands over is its own, in a shape its own world declares, and a host that
changes that shape is changing the world and owes its plugins a version bump.

## Consequences

- The high-value item in the spec's own table turns out to be blocked upstream,
  and the table should say so rather than continue to imply it is available.
- Reload's cost is a full reinstantiation: recompilation is avoided by the
  `.cwasm` cache, but the guest's own startup — CPython's, StarlingMonkey's —
  is paid again. For the 18 MB Python guest that is not free, and a host should
  reload because something changed rather than on a timer.
- A world that wants reload declares two more exports. Worlds that do not care
  are unaffected, and a plugin that does not export them reloads with no state,
  which is the honest behaviour rather than an error.
- If wasmtime ever exposes component memory, this ADR is the place that says
  what we would reconsider and why we did not simply wait.

## Addendum — 2026-09-06, on implementation

Six notes from building this. Left here rather than edited into the text above,
following ADR-0006 and ADR-0009: what a decision got wrong is worth more than a
tidy record of it.

**The hooks are `save-state` and `restore-state`, not `export-state` and
`import-state`.** The spec's wording was kept in the ADR body because it was the
spec's; the implementation should not keep it. In a component-model project
"export" and "import" already mean something exact, and `import-state` is an
*export* — a category error that costs the reader a beat every single time. The
names are top-level exports, because `Plugin::call` reaches top-level exports
and only those: a hook the untyped path cannot call would be no use to the C API
or the CLI. They carry no `watoots-` prefix, because the world declaring them is
the *application's*, and putting our brand in someone else's WIT buys nothing.
Both names are published as `watoots::SAVE_STATE_EXPORT` /
`RESTORE_STATE_EXPORT` and as `wt_save_state_export()` /
`wt_restore_state_export()`, so a world author reads them off the API rather
than out of prose.

**The shape is "one value of whatever type the world declares".** `save-state`
takes nothing and returns exactly one value; `restore-state` takes exactly one
parameter and returns nothing; the two types must be equal. watoots does not
look inside the value — it is a `Val` in transit — which is what lets one host
serve worlds whose state is a `u32`, a record, or a `list<u8>` without knowing
any of them. A type mismatch is refused by name rather than left to the
canonical ABI, and the error says what the ADR says: this is not cross-version
state migration.

**"The host can cap its size against `limits.transfer`" needs no code.** A
`save-state` call is an ordinary crossing, so wasmtime's hostcall fuel — which
`limits.transfer` sets — already bounds what the host allocates lifting the
value. 60 kB of state against `transfer = 1024` is refused with no special case,
which is the argument for a typed value making its own point.

**The order is: build the replacement first, then ask for the state.** The ADR
says a failed reload leaves the old instance running; putting the grant check
before the first call into the guest makes that stronger — a reload refused for
asking too much has not so much as entered the running plugin. It costs an
instantiation that is then dropped, and it means two instances exist at once for
the duration, which for the 18 MB Python guest is a real spike. Worth it: the
security-critical failure is the one that should cost the least.

**One failure does still cost the old instance, and it is not ours.** Wasmtime
48 refuses to re-enter a component instance after *any* trap. So a `save-state`
that traps leaves a plugin that was not replaced and also cannot be called —
unreplaced but uncallable. That is true of a trapping `lint` call too; reload
neither causes it nor can undo it. Every failure reachable without entering the
guest — a refused import, bytes that are not a component, a hook of an unusable
shape, a state type that does not match — leaves a fully working plugin, and
that is as far as "the old instance survives" honestly extends.

**Counters accumulate; the profile does not.** `PluginStats` answers "what has
this plugin cost me", and a reload is a new build of the same plugin rather than
a new plugin — a host reloading on every file change would otherwise report
near-zero fuel forever — so every counter carries across and a new `reloads`
field separates "one instance did all this" from "this is the fourth build".
`peak_memory_bytes` keeps the maximum, because a high-water mark that falls is
not one. `PluginProfile` starts again, because a profile attributes time to
*code* and one row averaging two builds of an export describes neither;
comparing the profile before a reload with the one after is the reason to
profile a reload at all. Sampled guest profiles cannot span a reload even in
principle — `GuestProfiler::new_component` binds a sampler to one compiled
component — so that half is structural rather than a preference.

**A note that is not about reload.** `limits.transfer` overruns arrive as
`ErrorKind::Trap`, not `LimitExceeded`, because wasmtime reports an exhausted
hostcall budget as a trap and watoots has no marker to downcast to the way it
does for `LogVolumeExceeded`. A ceiling reported as a misbehaviour is the wrong
answer, and it is the same distinction ADR-0006 drew for log volume. Left alone
here because it predates this work and belongs in its own change; recorded so it
is not rediscovered.

> **Fixed 2026-09-06.** It got its own change, as this note asked for.
> `exhausted_transfer_budget` recognises the exhaustion by matching wasmtime's
> message — there is still no public type to downcast to — and a transfer
> overrun is now `ErrorKind::LimitExceeded` carrying `Ceiling::Transfer`, which
> also means the audit trail sees it. Matching a rendered string is a real
> fragility, so both tests that exercise the ceiling assert the resulting kind
> as well as the message; the one in `tests/reload.rs` is built from WAT and
> runs even when no sample guest has been compiled. This paragraph is left
> standing rather than edited because the note doing its job — being found and
> acted on — is the argument for writing such notes at all.
