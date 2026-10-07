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
  recorded at all. Worlds without resources — like `examples/wit/lint/lint.wit` —
  are fully expressible today.
- We inherit WAVE's syntax decisions, including how it renders floats and
  strings. That is the point: it is a shared spelling, and diverging would cost
  more than it bought.
- Upgrading wasmtime means checking the `wasm-wave` pin moves with it.

## Addendum (2026-10-07): a typed path beside the text one

WAVE stays the format of the trace and of `wt_plugin_call`, and this decision
stands. What changed is that the C API is no longer *only* text.

The previewer made the cost concrete. A codec's argument is a file and its
result is a pixel buffer, and WAVE renders a `list<u8>` as four characters per
byte: `examples/README.md` measured a 2.6 MiB image as 11.5 MiB of argument,
12.5 MiB of answer, and ~125 ms of text conversion the profiler could not even
see. The C++ previewer carried a hand-written WAVE parser to read its pixels
back. That is the wrong shape for the audience this project is for, whose first
act with a plugin API is to pass a buffer.

`wt_val_t` is `wasmtime::component::Val` behind an opaque pointer -- a
constructor and an accessor per WIT kind, and `wt_plugin_call_vals`. It is not
a second serialisation: there is no format, the value is built in place and
handed to the same `Plugin::call` the WAVE path reaches after parsing. The two
paths therefore share limits, trace events and audit, and a recording made
through one replays through the other, which is the property that made this
safe to add without touching the trace format.

Two consequences, both deliberate:

- **Argument type checking moved before the call.** The WAVE path could not
  produce a mismatched `Val` because it parsed text against the parameter type;
  the typed path can, and wasmtime reports a mismatch from inside the call in a
  way that poisons the instance. `crates/host/src/typecheck.rs` walks each
  argument first, so a host's mistake is `InvalidArgument` and the plugin
  survives it.
- **The `Vec<Val>` cost stays.** A `list<u8>` is still one `Val` per byte on
  the host side -- 48 bytes each -- and `limits.transfer` is still measured in
  those. The typed path removes the text, not the dynamic representation; a
  Rust host that needs the last of it uses `bindgen!`, as before.

Host functions (`wt_host_func_t`) remain text-only. Their payloads in every
example are a level and a message, and a typed callback signature is a second
surface to add when a host has a reason, not before.
