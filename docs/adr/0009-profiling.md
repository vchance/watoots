# ADR-0009 — Profile at the boundary; the deadline stays authoritative

Date: 2026-09-04. Status: accepted.

## Context

`docs/SPEC.md` lists as a v0.2 candidate:

> Profiler view: wrap `GuestProfiler` and attribute time to guest vs.
> host-call vs. boundary marshalling per WIT function.

Those are two different things, and only one of them is Wasmtime's.

`GuestProfiler` produces a Firefox Profiler JSON from sampled guest stacks. It
answers "which guest function is hot". It cannot answer the question the second
half of that sentence asks, because the split between *guest*, *host call* and
*boundary marshalling* happens at a seam Wasmtime does not attribute — the
canonical ABI lift and lower, which is our layer.

That second question is the one a plugin-host user actually has. "My lint call
takes 40ms" has three very different causes: the plugin is slow, the host
function it calls is slow, or you are copying a large `list<diagnostic>` across
the boundary every call. The fix differs in each case and nothing today
distinguishes them.

## Decision

### Three buckets, measured at the boundary

`Store::call_hook` fires on every host↔guest transition with a `CallHook` —
`CallingWasm`, `ReturningFromWasm`, `CallingHost`, `ReturningFromHost`. That is
an exact signal, not a sampled one, and it gives two of the three buckets
directly:

- **guest** — between entering wasm and leaving it for a host call.
- **host call** — between `CallingHost` and `ReturningFromHost`.

The third is derived: **marshalling** is the wall time of `Plugin::call` minus
the other two. Lifting and lowering happen inside `Func::call` before the first
`CallingWasm` and after the last `ReturningFromWasm`, so what is left over is
the boundary. Derived rather than measured, and the ADR should say so: it also
absorbs our own dispatch overhead. That is honest for the question being
asked — "is my time going into copying?" — and it must be documented as a
remainder rather than presented as a measurement.

Attribution **per WIT function** needs no new bookkeeping. `Plugin::call`
already knows which export it is in, and the logging and host-function shims
already know which import they are serving; the same seam `TraceHook` sits on.

### The deadline stays authoritative

`GuestProfiler::sample` is meant to be driven from
`Store::epoch_deadline_callback`, and watoots already uses the epoch deadline
for `limits.timeout`. They have to share it: the callback sets the next
deadline, so whichever one sets it wins.

**The timeout wins.** The callback samples, then continues with a deadline of
`min(sample interval, remaining timeout)`, and traps when the timeout is spent.
Sampling must never extend a plugin's life — a profiler that lets a runaway
guest run forever has defeated the thing this project is for. The remaining
budget is tracked explicitly rather than inferred from the deadline, because
the deadline is now being reused.

### Off by default, and never on during record or replay

Profiling changes timing, and timing is exactly what the determinism knobs
exist to pin. A trace recorded under a profiler would record a run nobody else
can reproduce. So profiling is opt-in per plugin, and enabling it alongside
`record` is refused rather than silently permitted.

### Where it lands

- `Plugin::profile()` returning the three buckets and per-function totals,
  alongside `PluginStats` — same rationale as ADR-0006: observed at the
  boundary, so the guest can neither forge nor inflate it.
- A C surface, because plain integers cross without argument and a Rust-only
  accessor recreates the drift already fixed twice.
- `watoots profile <component> -m <manifest> -c <export>` printing the split,
  with `--firefox <file>` writing `GuestProfiler`'s JSON for the guest-stack
  question. That is where the wrapping the spec asked for happens, and it is
  the optional half.

## Consequences

- We take on the epoch deadline being used for two purposes. That is the risk
  in this ADR, and it sits on the path that stops runaway plugins, so the
  timeout behaviour needs a test that fails if sampling ever lets a plugin
  outlive its budget.
- The marshalling number is a remainder and will absorb any future overhead we
  add to `Plugin::call`. It is a diagnostic, not an accounting identity.
- A profiled run is not a determinism-preserving run, which is stated rather
  than worked around.
- `GuestProfiler` needs the component's `Module`s, so profiling requires the
  component to be compiled rather than loaded from the `.cwasm` cache; that
  path may need a plain compile.

## Addendum — 2026-09-04, on implementation

Four notes from building this. Left here rather than edited into the text
above, following ADR-0006: what a decision got wrong is worth more than a tidy
record of it.

**"That path may need a plain compile" — it does not.** A component
deserialized from a `.cwasm` carries its static core modules just as a freshly
compiled one does, so `GuestProfiler::new_component` works off the cache with
no special case. `a_component_loaded_from_the_cwasm_cache_can_still_be_sampled`
is the test that says so rather than leaving the guess standing.

**`Store::call_hook` is behind a non-default Cargo feature.** `call-hook` is
not in Wasmtime's default set — only `memory-protection-keys` pulls it in — so
`crates/host` now asks for it explicitly. Wasmtime's own comment says the
feature "imposes a dynamic dispatch on the store trait object which otherwise
can't be optimized away"; measured on an empty host function, turning it on
with no hook installed is inside the run-to-run noise (~86ns per host call
either way). It is a feature flag, not a new dependency.

**The wasm window is the outer one, not the inner one.** `CallingWasm` and
`ReturningFromWasm` bracket the whole of `Func::call`'s excursion into wasm,
*including* the host calls made from inside it — they are not per-guest-frame.
So `guest` is that window minus the host windows, rather than a sum of
disjoint intervals, and a host function that called back into the guest would
count the re-entry as host time. watoots installs no such host function; the
depth counters keep the arithmetic sane rather than exact if one appears.

**Per-function attribution is complete for exports and partial for imports.**
`Plugin::call` names the export and the shims name the import, as the ADR
said. But the call hook fires for *every* host call, and the `wasi:`
interfaces are implemented by `wasmtime-wasi` rather than by a watoots shim —
so their time is in the `host` bucket with no row to put it in. The rows are a
subset of the bucket by construction, and both the rustdoc and the CLI say so
rather than leaving someone to work out why the arithmetic does not close.

**Cost.** Profiling off is unchanged: no call hook and no epoch callback are
installed. Profiling on costs roughly 80ns per `Plugin::call` and 90ns per
host call. Adding guest samples costs another ~150ns per host call (each
`CallingHost` takes a backtrace) and, on a compute-bound guest, about a third
of its run time at a 1ms interval. That is the argument for `profile()` and
`profile_guest_samples()` being separate: the boundary split is cheap enough
to leave on, and the sampler is not.
