# ADR-0013 — Stay on WASI 0.2 for the host, and say what would change that

Date: 2026-09-06. Status: accepted.

## Context

`docs/SPEC.md` has carried this since M1: ship on 0.2, keep the permission model
0.3-shaped, don't depend on `wasmtime-wasi::p3`. It also carried, in the same
paragraph, an admission that this "is no longer the consensus position and the
spec should not pretend it is", followed by "a p3 position needs writing down".

This is that. The facts below were re-checked at source in September 2026,
because several of the spec's were from late August and two had drifted.

### What `wasmtime-wasi` 48.0.1 actually says

`src/p3/mod.rs`, verbatim and complete:

> Experimental, unstable and incomplete implementation of wasip3 version of WASI.
>
> This module is under heavy development. It is not compliant with semver and is
> not ready for production use.
>
> **Bug and security fixes limited to wasip3 will not be given patch releases.**
>
> Documentation of this module may be incorrect or out-of-sync with the
> implementation.

The module is behind `#[cfg(feature = "p3")]` and covers `cli`, `clocks`,
`filesystem`, `random` and `sockets`. There is no `http` in it.

### What the guest toolchains can do

| | |
|---|---|
| wasi-sdk 34 | ships a `wasm32-wasip3` sysroot — **verified on this machine** |
| Rust 1.97.1 (our toolchain) | `rustup target list` does not offer `wasm32-wasip3` |
| `wasm32-wasip3` tier | **Tier 3**, per the rustc book's platform-support page |

The spec said the target "was accepted for Rust Tier 2 on 2026-08-09". That is
right about the *proposal* — rust-lang/compiler-team#1001 closed that day with
`disposition-merge` after FCP — and wrong as a statement about the target, which
the rustc book still documents as Tier 3. Accepted for promotion is not
promoted. The spec's line is corrected.

Also corrected: the spec credited "Spin 4.0". It is Spin **4.1.0** that bumps to
Wasmtime 48.0.0 and carries p3 through core features including HTTP middleware
and service chaining. The substance holds — a production framework ships p3 on
our exact engine pin — but the version was wrong and this ADR is where such
claims get checked.

wasmCloud's own docs, quoted: "Starting in wasmCloud 2.5.0, the runtime is built
with WASI 0.3 always on (on Wasmtime 46 in the 2.5.x releases, and Wasmtime 47
as of wasmCloud 2.6.0)", and "Components targeting either P2 or P3 worlds are
compatible with the runtime." So the two most-cited adopters are not doing the
same thing we would be: wasmCloud runs p3 on 46/47 rather than the 48 LTS, and
serves *both* worlds at once rather than switching.

## Decision

**The host stays on WASI 0.2. p3 is not adopted, and the conditions for
adopting it are written down rather than left to judgement.**

The disqualifying fact is one sentence: *bug and security fixes limited to
wasip3 will not be given patch releases.* Everything watoots sells is a sandbox.
A dependency that will not ship security patches on its release line is not a
dependency a sandbox can take, and no amount of ecosystem momentum changes that
arithmetic. Spin and wasmCloud can carry it because they ship the whole stack
and can move engines on their own schedule; watoots is a library that a host
application pins, and its users inherit whatever we depend on.

That this is the *only* disqualifying fact is worth stating plainly. Not
"experimental" — most of what this project builds on was experimental once.
Not incompleteness — we use a subset. The patch-release policy is the one that
cannot be worked around by being careful.

### It would be adopted when both of these are true

1. `wasmtime-wasi`'s p3 module no longer says security fixes are excluded from
   patch releases, and no longer says "not ready for production use".
2. `wasm32-wasip3` is Tier 2 in the rustc book and installable via `rustup` on
   our MSRV — so that the Rust sample guests, which are the ones CI builds, can
   actually target it.

Both are checkable in a minute, by a person or a script. Neither is a matter of
taste. The first is the security gate; the second is the "can we test it" gate,
and a p3 path we cannot exercise in CI is a p3 path we would be guessing about.

wasi-sdk 34 already shipping a p3 sysroot while rustup does not offer the target
is the asymmetry to watch: the C++ guest could target p3 today and the Rust
guest could not, which would mean a conformance suite where the four guests no
longer implement the same thing. That is a good reason to move all at once.

### Meanwhile, "0.3-shaped" stops being an intention and becomes a test

`CLAUDE.md` has claimed since M1 that the permission model is kept 0.3-shaped so
the upgrade is additive. Nothing checked it. `crates/host/tests/wasip3_shape.rs`
now does, against interface names read out of `wasmtime-wasi` 48.0.1's own
`src/p3/wit/deps/*.wit`.

The claim mostly held. Grants are matched with the version stripped, so
`wasi:filesystem/types@0.3.0` and `@0.2.12` classify identically; `wasi:io`
disappearing in 0.3 costs nothing because it was `Ambient` and conveyed no
capability; the new `wasi:random/insecure` and `insecure-seed` land on `Random`
correctly.

**It did not hold for clocks, and that was a live bug.** WASI 0.3 renames
`wall-clock` to `system-clock` — the same real-time clock, "intended for
reporting the current date and time for humans" — and adds `timezone`. The table
matched `("clocks", "wall-clock")` and then fell through on `("clocks", _)` to
`MonotonicClock`. So under 0.3 the *real-time clock* would have been classified
as the monotonic one, and a manifest saying `clocks = "monotonic"` — the setting
someone picks precisely to keep real time away from a plugin — would have
admitted it.

`clocks` is the only WASI package split across two different capabilities, which
is exactly why a fallthrough there is dangerous in a way the `("filesystem", _)`
and `("sockets", _)` fallthroughs are not: those name the capability that covers
the whole package. The clocks arm is now exhaustive, unknown `clocks` interfaces
deny, and both spellings of the wall clock map to `WallClock`.

It was latent rather than exploitable: the host links wasip2 only, so a p3
component fails at instantiation regardless. But classification runs *before*
that, so `watoots inspect` — the tool whose entire job is telling you what a
plugin may do before you install it — would have answered wrongly, in the
direction of over-granting. For this project that is the worst place to be
wrong.

## Consequences

- The `Wasmtime 48.x` / `WASI 0.2.x` constraint in `CLAUDE.md` stands, now with
  a reason and an exit condition rather than a bare instruction.
- Someone can check quarterly whether the two conditions have become true
  without re-deriving the argument. That is the point of writing it down.
- `wasip3_shape.rs` will fail if a future edit reintroduces a fallthrough that
  mis-sorts a p3 interface. The 0.3-shaped claim is now falsifiable.
- The p3 interface list is pinned to what 48.0.1 ships. WASI 0.3.x is still
  moving; a later engine may add interfaces this test does not know about, and
  they will classify as `Unrecognized` and deny — which is the correct failure
  and also the signal to revisit.
- Adopting p3 later is still additive for manifests: no key changes, no grant
  changes. The measurement says so.
- **Not decided here:** whether to *serve* p3 alongside p2 from one host. That
  is a linker question and a bigger one, and it is what wasmCloud actually does
  — components targeting either world run on one runtime. It is probably the
  shape watoots would want too, since a plugin host cannot make every plugin
  author migrate on the same day. Worth its own ADR when condition 1 above
  clears; the capability model is ready for it either way, because grants are
  matched with the version stripped and `wasip3_shape.rs` now proves that.
