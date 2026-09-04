# ADR-0008 — Fuzz with proptest on stable, and make record/replay the oracle

Date: 2026-09-04. Status: accepted.

## Context

`docs/SPEC.md` lists as a v0.2 candidate:

> WIT-driven fuzzer reusing Wasmtime's `component_api` oracle pattern (or
> `mutatis`), emitting a replay trace per crash.

Two questions hide in that line, and the second is the interesting one.

**What runs the fuzzer** — `cargo-fuzz`, `mutatis`, or property tests.

**What counts as a crash.** A plugin host has no opinion about what a plugin
*should* return, so "wrong answer" is not available as a signal. Without an
oracle a fuzzer can only find panics, which is a thin result for the effort.

## Decision

### proptest, on stable, inside `cargo test`

`cargo-fuzz` requires a nightly toolchain. This project pins MSRV to 1.95
stable because that is what Wasmtime 48 requires, CI runs
`dtolnay/rust-toolchain@stable`, and `CONTRIBUTING.md` tells contributors that
stable is enough. Adding a nightly toolchain to the gates so that one target
can run is a poor trade for a project maintained by one person in evenings.

proptest runs under `cargo test`, so the properties are in CI from the first
commit rather than in a campaign somebody remembers to launch. It shrinks
failures automatically and persists them in a regression file, which is the
same instinct as `replay --emit-test`: a failure becomes a fixture. And
[ADR-0003](0003-cpp-toolchain.md) already anticipated it — "`proptest` for
binary↔WAVE round-tripping when M4 lands".

This is not a permanent no to `cargo-fuzz`. Byte-level fuzzing of the *parsers*
— malformed components, manifests and traces — is a different target with a
different shape, and libFuzzer is better at it. That target can be added later
without disturbing this one, because it shares no code with the WIT-driven one.

### The oracle is record/replay self-consistency

For any component and any sequence of calls, **recording a session and then
replaying it must report faithful**. That is the property, and it is the right
one for three reasons:

- **It needs no expectation of guest behaviour.** It compares watoots against
  watoots, so it works on a plugin nobody has ever seen.
- **It tests the differentiator.** Record/replay is what `docs/SPEC.md` calls
  the highest-value piece. A fuzzer that exercises the manifest parser would be
  fuzzing the least interesting code in the repository.
- **Every failure is already a trace.** "Emit a replay trace per crash" is not
  extra work here; the artifact is the thing that failed. `watoots replay
  --emit-test` turns it into a Rust regression test with no host code, which
  closes the loop the spec asked for.

Four properties, in dependency order:

1. `Val` → WAVE → `Val` is the identity, for every type WAVE can spell.
2. A trace survives text → binary → text unchanged.
3. Recording a call sequence and replaying it reports faithful.
4. No generated input makes the host library panic. A `Result::Err` is a fine
   answer; a panic is a bug even where `catch_unwind` at the C boundary would
   contain it.

### Generation is driven by the component's own types

The "WIT-driven" part: arguments come from `Func::ty()`, so a generated call is
type-correct by construction. Fuzzing a component with random *bytes* would
spend its whole budget being rejected by the canonical ABI — the lifting code
is Wasmtime's and already fuzzed upstream. What is unfuzzed is our layer above
it, and reaching it requires valid values.

Resource handles, futures and streams are out of range, for the reason
[ADR-0004](0004-wave-and-dynamic-typing.md) gives: they have no WAVE spelling,
so a generator that produced them would be generating values the trace format
cannot record. The generator refuses them explicitly rather than producing
something that fails downstream for an unrelated-looking reason.

### A `watoots fuzz` subcommand for campaigns

The properties above cover components the tests construct. A user wants to
point the thing at *their* plugin, so `watoots fuzz <component> -m <manifest>`
runs the same oracles against a real one, writes `crash-NNN.wave` on failure,
and prints the `replay --emit-test` line that turns it into a test. The
subcommand and the property tests share the generator; neither reimplements the
other.

## Consequences

- The gates grow a fuzzing budget. Property tests must stay fast enough that
  `cargo test --workspace` is not something contributors dread — a bounded case
  count by default, with a longer campaign available through `watoots fuzz`.
- proptest and its regression files become part of the repository. A committed
  `.proptest-regressions` file is a found bug that must keep being caught, and
  should be read as such in review rather than as noise.
- A component with resources in its world cannot be fuzzed, exactly as it
  cannot be traced. The limit is the same one, stated once.
- We do not get libFuzzer's coverage guidance. For structure-aware generation
  against a handful of exports that is an acceptable loss; for parser fuzzing
  it would not be, which is why that target stays open.

## Addendum — 2026-09-04, on implementation

**The generator is byte-driven, not proptest-driven.** `Generator::from_bytes`
takes a finite buffer and reads zero once spent. proptest hands it bytes it can
shrink; `watoots fuzz` hands it bytes expanded from `--seed`. That was not what
this ADR pictured, and it is better: one implementation serves both, generation
always terminates, a shorter buffer yields a simpler value, and `proptest`
stays out of the published library's dependency graph entirely — it is a
dev-dependency of `crates/host` and `crates/trace` and appears in neither
crate's normal graph.

**The out-of-range list was incomplete and slightly wrong.** `map` and
`error-context` are out for the same reason as resources, futures and streams;
the generator refuses all five by name. But `fixed-length-list` is *in* range —
wasmtime's wave implementation gives it a `WasmTypeKind`.

**`wasmtime::component::Type` has no public constructor.** Any property over
"every type WAVE can spell" has to source its types from a compiled component,
so the tests carry a WAT "type zoo": an imported instance whose function
parameters cover the spellable types, compiled once and never instantiated.

**A trapped instance refuses re-entry,** so a generated call sequence stops at
its first failure rather than continuing.

### What the properties found

Four defects on the first two runs, all left unfixed and each pinned by a named
test, so review sees the defect rather than a generator that quietly avoids it:

1. **WAVE reorders flag sets** — upstream, in `wasm-wave 0.254`, which collects
   labels into a `BTreeMap` and returns `into_values()`. `Val` compares flags
   positionally, so the round-trip fails for any set of two or more flags not
   declared alphabetically. Record/replay is unaffected: traces compare
   rendered text, and lowering maps labels to bits.
2. **The text encoding emits a line its own parser rejects** — an argument
   whose value is the empty string renders as `  arg ` and fails to re-parse.
3. **Whitespace-padded values are silently trimmed.**
4. **The manifest header is normalised** — a `manifest_toml` without a trailing
   newline gains one. Unlike 2 and 3 this *is* reachable from a real recording,
   since `record -m policy.toml` stores the file verbatim and TOML files
   without a final newline are ordinary. It is lossless where it counts, which
   is why it was left.

2 and 3 are unreachable from a recording but reachable by hand-editing a trace,
which is a thing the text format exists to invite. The binary framing has none
of them.

### The oracles were checked, not assumed

A fuzzer that finds nothing on its first run is a suspicious result, so three
deliberate mutations were made and reverted. The decisive one: changing
`replay`'s export-return lookup from `events[index + 1..]` to `events[..]`
left **all sixteen existing fixed record/replay tests passing** and failed
property 3. That bug only appears when one export is called more than once in a
session, which no fixed test does. That single result is the argument for
having written this.

## Addendum — 2026-09-04, on fixing what it found

Three of the four defects are fixed; the fourth is upstream.

**The manifest header keeps its bytes.** `to_text` used `lines()`, which cannot
tell `"a\nb"` from `"a\nb\n"`. `split('\n')` can: it yields an empty tail
element for a trailing newline, and `join("\n")` puts it back. One consequence
is worth knowing — a manifest ending in a newline now emits a final line of two
spaces, so an editor that strips trailing whitespace drops the newline again.
That is lossless as far as `Manifest::parse` is concerned, which is the only
thing that reads the header back.

**An `arg` or `value` line carries its value verbatim.** Indentation is
trimmed, the value is not, and a bare `arg` is the empty string rather than a
parse error. The encoder can no longer emit a file its own parser refuses.

**The filter that dodged the bug is gone**, which is the part that matters. A
generator that excludes the values a defect lives in stops finding it, so
`awkward()` now produces empty and padded values.

Removing it immediately found a fifth thing, and it is a different kind. A
value containing `\r` cannot survive a line-oriented encoding — values are
written unquoted because they are already WAVE, and quoting would
double-escape. Nothing can put one there: `to_wave` escapes control characters.
So this is a contract rather than a bug, and the distinction is the reason
`a_value_cannot_contain_a_raw_line_break` exists as an explicit test. One
filter remains in `awkward()`; it is that contract, and it says so.

**No `.proptest-regressions` file is committed.** The campaign produced one, for
the `\r` case, and it was deleted rather than checked in: the generator now
filters that value, so replaying the seed yields a rejected case and never a
failure. A saved seed that cannot reproduce is worse than no file, and the case
is pinned by a named test instead. The consequence recorded above — that a
regression file is a fixture to read in review — stands for seeds that still
reproduce.

**WAVE's flag reordering is upstream and stays.** `wasm-wave 0.254` collects
flag labels into a `BTreeMap`; fixing it means a patch to the Bytecode
Alliance, not to this repository. Record/replay is unaffected, since traces
compare rendered text and lowering maps labels to bits.

