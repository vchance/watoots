# What it costs

Measured, not estimated. `cargo bench -p watoots --bench boundary` reproduces
everything here; the source is `crates/host/benches/boundary.rs` and each case
runs a guest that does as little as possible, so the number is watoots' overhead
rather than somebody's lint rules.

**These numbers are one machine on one day.** They are here so the shape of the
costs is public, not so anyone can quote them. Re-run the benchmark on your own
hardware before making a decision that depends on a specific figure.

| | |
|---|---|
| Machine | Apple M4, macOS 26.6.2 |
| Toolchain | rustc 1.97.1, release profile |
| Engine | Wasmtime 48.0.2 |
| Date | 2026-09-11 |

## One call across the boundary

```
crossing/call/no-limits      337 ns
crossing/call/fuel           321 ns
crossing/call/timeout        333 ns
crossing/call/fuel+timeout   317 ns
```

**Read this as "about 330 ns, and the limits make no measurable difference."**

Do not read the ordering. Fuel cannot make a call faster, and it appears to here
in every run — the spread is about 5%, it is reproducible rather than random,
and the likely cause is that each case builds its own `Engine` with its own
`Config`, so the guest is compiled differently and lands differently in cache.
Whatever the cause, a difference that points the wrong way is a difference the
measurement cannot resolve.

The useful conclusion is the one that survives: **`[limits]` costs nothing per
crossing.** Its cost is per *instruction*, which is why it needs its own
benchmark.

## What the limits actually cost

100 000 iterations of a counting loop, so there is something to meter:

```
metering/spin/no-limits     24.0 µs
metering/spin/fuel          48.9 µs      ~2.0x
metering/spin/timeout       40.6 µs      ~1.7x
```

Fuel roughly doubles guest execution time. An epoch deadline adds about 70%.

**Both figures are close to a worst case.** A tight counting loop is the densest
possible arrangement of the things these mechanisms check: fuel is charged per
instruction, and epoch interruption inserts a check at every loop back-edge. A
guest that does real work between back-edges pays the same absolute overhead
spread over more useful instructions, so the ratio falls. A guest that mostly
waits on the host pays almost nothing.

That is still a real price, and it is the argument for `timeout` over `fuel`
where either would do: a deadline costs less and answers the question most hosts
actually have, which is "stop it if it hangs" rather than "stop it after exactly
this much work". `fuel` earns its cost when you need the *deterministic* bound —
the same input must stop at the same place on every machine, which a wall-clock
deadline cannot promise. See `docs/MANIFEST.md`.

## Marshalling

```
marshalling/call/string          400 ns
marshalling/call_wave/string     640 ns
```

A short string in and out costs about 65 ns over the empty call. Going through
WAVE text adds about **240 ns** — parsing the argument and rendering the result
— which roughly doubles a small call.

That is the cost of the dynamic path, and it is worth what it buys: a C host, a
command line, and a trace that a human can read and edit. A Rust host with a
static world can use `bindgen!` and skip it. It is also the cost that was
invisible until the profiler grew a `wave_nanos` bucket, which is why that bucket
exists.

## Loading

```
loading/load/cold     166 µs      first load: compile + instantiate
loading/load/warm      38 µs      same bytes again: instantiate only
```

The gap — about 130 µs, or 4.4x — is what the in-memory compiled-component cache
saves on every instance after the first. For this trivial component the absolute
saving is small; for a real guest it is the difference between a plugin host that
can open a document per tab and one that cannot. The sample Python guest is 18 MB
of CPython, and compiling it is measured in seconds rather than microseconds.

`cache_dir` saves the same work across *process restarts*, where the in-memory
cache cannot help. See `Host::builder().cache_dir`.

## What is not measured here

- **Guest execution.** Everything above uses guests chosen to do nothing, on
  purpose. What your plugin costs is what your plugin costs.
- **Recording.** `TraceHook` writes a value per crossing; a recorded session is
  not the shape you should benchmark against production.
- **Signature verification.** One ECDSA P-256 verification per *load*, never per
  call.
- **Real-world guests.** `tools/demo.sh` and `crates/host/tests/asset_e2e.rs`
  exercise 14 MB JavaScript and 18 MB Python components, which behave nothing
  like these microbenchmarks.
