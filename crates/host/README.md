# watoots

A sandboxed plugin host for native applications on the WebAssembly component
model, built on [Wasmtime](https://wasmtime.dev) 48 (LTS). This is the core
crate: the `Host`, the `Plugin`, the manifest that says what a plugin may do,
and the dynamic call path everything else is built on.

```rust
use watoots::{Host, Val};

let host = Host::builder().manifest_from_file("policy.toml")?.build()?;
let mut plugin = host.load("decoder.wasm")?;
let out = plugin.call("decode", &[Val::List(bytes.into_iter().map(Val::U8).collect())])?;
```

```toml
# policy.toml -- everything absent is denied
[permissions]
clocks = "monotonic"

[limits]
memory  = "64MiB"
fuel    = 200_000_000     # per call
timeout = "2s"            # per call

[signature]
required = false          # or `keys = [...]`; a policy file must say which
```

What the crate does for you:

- **Default-deny, checked at load.** A component declares its imports in the
  binary. The manifest is intersected with them before anything runs, and a
  plugin that wants a capability you did not grant is refused with the import
  named -- an install-time error, not a 3am trap.
- **Per-call ceilings** on memory, fuel, wall time and host allocation, each
  reported as the ceiling it was (`ErrorKind::LimitExceeded`, "the plugin asked
  for 1074921472 bytes, the manifest allows 67108864").
- **Pinned-key signatures** in the format `cosign sign-blob` writes, verified
  at load and at reload.
- **An audit trail** of every authorisation decision, with no argument values
  in it, so it is safe to keep.
- **Record/replay hooks**: every crossing goes through `Plugin::call`, so a
  `TraceHook` sees all of them. The `watoots-trace` crate turns that into a
  readable file that replays without your application.
- **Reload** with state carried across as a WIT value, a boundary profiler,
  and a compiled-component cache.

Values cross as `wasmtime::component::Val` or as WAVE text (`"notes.md"`,
`{line: 2}`); a Rust host with a static world can use `bindgen!` against the
same engine instead.

The C API is `watoots-capi`, the command line is `watoots-cli`, and the
repository's [README](https://github.com/vchance/watoots#readme) has the whole
story, including the previewer whose format decoders are untrusted plugins and
the file that makes one of them ask for a gigabyte.

Licensed under Apache-2.0 WITH LLVM-exception.
