# watoots-trace

WIT-level record/replay of the crossings between an application and a
[watoots](https://crates.io/crates/watoots) plugin.

A recording is a text file: the component's hash, the manifest it ran under,
and every call across the boundary in both directions as WAVE values.

```
watoots-trace 1
component sha256:2e79da2b...
plugin rust_qoi
manifest
  [limits]
  memory = "64MiB"
export-call decode
  arg [113, 111, 105, 102, 0, 0, 64, 0, ...]
export-return decode
  error WT_ERR_LIMIT_EXCEEDED "rust_qoi: decode: limits.memory: the plugin asked for 1074921472 bytes, ..."
```

Replaying needs the trace and the component, and nothing else: the manifest
travels in the header, and calls the plugin made *into* the application are
answered from the recording in place of your code. A bug report becomes a
file; `--emit-test` turns the file into a regression test with no host code
around it.

This crate is the format (text and a binary framing), the recorder that is
installed as a `TraceHook`, and the replay runner. The `watoots` command line
drives it: `watoots record`, `watoots replay`, `watoots trace fmt`.

It is deliberately not Wasmtime's own `rr`, which records at the canonical-ABI
level for bit-exact engine determinism in a format that is not meant to be
read. This is the human-readable layer that work names as an explicit
non-goal; the two compose.

Licensed under Apache-2.0 WITH LLVM-exception.
