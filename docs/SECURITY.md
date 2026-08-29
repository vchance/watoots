# What this protects against, and what it does not

watoots runs untrusted plugins. That claim is only worth making if the limits of
it are written down, so here they are.

## What the sandbox gives you

- **No ambient authority.** A component cannot open a file, a socket, or read
  the clock unless the manifest granted the interface. This is enforced by
  WebAssembly itself: the guest has no syscalls, only the imports it was linked
  against.
- **Load-time verification.** A plugin's imports are declared in its binary and
  checked against the manifest before instantiation. An ungranted capability is
  a load error, not a runtime trap, so it is visible at install time.
- **Memory isolation.** Guest linear memory is a bounds-checked region. A plugin
  cannot read or corrupt host memory, or another plugin's.
- **Per-call resource ceilings.** Fuel, an epoch deadline, and a memory limit,
  re-armed before each call and held per plugin.

## What it does not give you

**Timing and side channels.** Nothing here defends against a plugin inferring
information from how long its own work takes, or from cache behaviour. If you
run mutually distrusting plugins whose *existence* is confidential, watoots is
not the boundary you want; use processes.

**Fair scheduling across plugins.** Limits are per plugin and per call. A host
that loads a hundred plugins and calls them all gets a hundred times the
ceiling. Budgeting across plugins is the application's job.

**Protection from the host functions you write.** Every interface you serve is
attack surface you own. watoots passes arguments to your callback; whether that
callback then reads a path the manifest never granted is up to you.

**A trustworthy cache directory.** `cache_dir` holds precompiled machine code
that the engine loads without re-validating. Write access to it is equivalent to
code execution in the host process. There is deliberately no default location —
point it somewhere your application controls, never somewhere world-writable.

**Protection against a malicious *component author* combined with a generous
manifest.** `fs.read = ["/"]` grants the filesystem. The tool tells you what a
plugin asked for; deciding whether to say yes is still a human judgement.

**Wasmtime's own bugs.** Sandbox escapes are found and fixed — a filesystem
escape and a heap DoS were both fixed in August 2026. We pin the Wasmtime 48 LTS
line, which receives patches for 24 months, and track its advisories. You still
have to update.

## Known gaps in v0.1

- **The `net` allowlist is not enforced.** A non-empty `permissions.net` is
  refused at host-build time rather than silently over-granting. `net = []`
  grants the socket interfaces while every connection is refused, which is what
  a CPython or JavaScript guest needs. Naming reachable hosts comes later.
- **Filesystem grants are directory-granular.** WASI preopens directories, so a
  grant admits the tree beneath it. The glob in a manifest path is expanded as a
  path, not applied as a filter.
- **Resource handles cannot be recorded.** A world that passes resources across
  the boundary cannot be traced; recording fails loudly rather than writing down
  a handle that means nothing on the way back in. See
  [ADR-0004](adr/0004-wave-and-dynamic-typing.md).
- **No host-to-guest reentrancy.** A host function that calls back into the same
  plugin is out of scope for v0.1 and is documented rather than half-supported.

## Trusted computing base

Running a plugin trusts: Wasmtime and its Cranelift backend, `wasmtime-wasi`,
this crate, your own host functions, your manifest, and anything in your cache
directory. It does *not* need to trust the plugin, its author, or its build
toolchain.

## Reporting a vulnerability

> **Not yet set up.** This project has no published security contact. Before
> v0.1 is announced anywhere, add one here — a private disclosure channel, and
> a statement of how quickly you intend to respond.

Please do not open a public issue for a suspected sandbox escape in Wasmtime
itself; report those to the Bytecode Alliance directly.
