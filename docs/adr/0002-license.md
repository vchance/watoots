# ADR-0002: License is Apache-2.0 WITH LLVM-exception

**Date:** 2026-08-28  
**Status:** accepted (revisable before the first real release)

## Context

Reserving the crate names on crates.io requires a `license` field, so the
decision was forced earlier than planned. The scoping spec already recommended
Apache-2.0 with the LLVM exception: it is what Wasmtime and the Bytecode
Alliance projects use, and the exception removes attribution friction for
applications that statically link the host library — exactly the C/C++
embedders we want.

## Decision

`Apache-2.0 WITH LLVM-exception` for all crates. `LICENSE-APACHE` and
`LICENSE-LLVM-EXCEPTION` at the repo root and packaged into each crate.

## Consequences

- Compatible with embedding in proprietary applications; no copyleft.
- Matches the ecosystem, so contributors and adopters see nothing unusual.
- Published v0.0.0 placeholders carry this license permanently; a change before
  0.1 would apply to later versions only.
