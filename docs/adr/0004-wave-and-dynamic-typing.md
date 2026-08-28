# ADR-0004 — Host-side dynamic typing: WAVE, and what it cannot say

Date: 2026-08-28. Status: accepted.

## Context

`docs/SPEC.md` left this open:

> **Host-side dynamic typing.** WAVE for the C API and CLI keeps us off the
> bindgen treadmill; confirm WAVE handles resources well enough for our traces
> before committing.

The C API cannot carry generics, and the CLI has strings rather than typed
values, so both need a dynamic path: `Val` plus a text encoding. WAVE is the
Bytecode Alliance's text syntax for component-model values, which makes a
diagnostic read as `{line: 2, message: "unresolved TODO"}` rather than as a
blob. The question was whether it is complete enough to build a trace format on.

## Decision

**Adopt WAVE, via wasmtime's own implementation.** Wasmtime 48 implements the
`wasm-wave` traits for its `Val` and `Type` behind its `wave` feature, so
`crates/host/src/wave.rs` is a wrapper that puts our error type on the front
and nothing else. `Plugin::call_wave` takes and returns WAVE strings, deriving
each parameter's type from the function's own signature.

This needs one pin: wasmtime 48 depends on `wasm-wave 0.254`, and depending on
a semver-incompatible version here would compile two copies of the traits, so
the impls would not apply. `wasm-wave = "0.254"` is load-bearing, not casual.

**The answer to the open question is no: WAVE cannot represent resources.**
Wasmtime's implementation maps `Own`, `Borrow`, `Stream`, `Future`,
`ErrorContext` and `Map` to `WasmTypeKind::Unsupported`, and rendering a
`Val::Resource` returns `UnsupportedType` outright.

That is not a gap to work around, and we should not invent a WAVE spelling for
handles. A resource handle is an index into a live table in one process. It is
not a value: writing `42` into a trace file records something that cannot mean
anything on the way back in, and would read as data while behaving as a
pointer. So:

- Traces carry resources as **stable trace-local IDs recorded beside the WAVE
  text, never inside it** (M4). The spec already anticipated this — "resource
  handles mapped to stable IDs" is in the record/replay scope.
- `to_wave` on a resource is an error today, deliberately, rather than
  something lossy that looks like it worked.
- v0.1 already scopes out host-to-guest reentrancy for the same family of
  reasons; the limit is documented rather than half-supported.

## Consequences

- The C API and CLI get a readable, diffable value encoding with no bindgen
  step and almost no code of ours to maintain.
- A trace of a world using resources needs the M4 side-channel before it can be
  recorded at all. Worlds without resources — like `examples/wit/lint.wit` —
  are fully expressible today.
- We inherit WAVE's syntax decisions, including how it renders floats and
  strings. That is the point: it is a shared spelling, and diverging would cost
  more than it bought.
- Upgrading wasmtime means checking the `wasm-wave` pin moves with it.
