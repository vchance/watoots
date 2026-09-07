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
- **Per-call resource ceilings.** Fuel, an epoch deadline, a memory limit, and a
  log-volume ceiling, re-armed before each call and held per plugin.
- **A ceiling on what a guest may hand back.** `limits.transfer` bounds the
  allocation the *host* makes while lifting a guest's arguments or return value,
  so a plugin cannot make the host allocate without bound on its behalf. Per
  crossing, and guest-to-host only — data going the other way is already
  resident here. See `docs/MANIFEST.md` for why its unit is not the byte count
  you would expect.

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
callback then reads a path the manifest never granted is up to you. The
`wasi:logging` sink is the same deal in a place that looks safer than it is: the
context and message are guest-controlled text of guest-chosen length, so treat
them as data — never as a format string — and remember that whatever you forward
them to inherits the exposure. `limits.log_bytes` and `limits.log_messages` bound
the volume; they say nothing about the content.

**An audit trail you did not install.** watoots keeps no record of what it
decided unless the embedding application registers an audit hook —
`HostBuilder::audit_hook`, `wt_host_builder_audit_hook`,
`wt::HostBuilder::AuditHook`. With no hook there is no file, no default sink and
no ring buffer anywhere: a refused load, a log line dropped by the level
ceiling, a spent `[limits]` ceiling and a refused reload are reported to the
caller that asked and are then gone. That is deliberate — a library that writes
to stderr uninvited is badly behaved, and the destination is the application's
choice — but it means an incident can only be reconstructed from decisions
somebody arranged to keep *before* it. Install one. `watoots run --audit` and
its sibling subcommands do it for you at the command line. See
[ADR-0011](adr/0011-audit-trail.md).

Two things the trail is not. It carries **names and verdicts only**, never
argument values or log message bodies, which is what makes an audit line safe to
paste into an issue when a trace is not — so it will tell you that a plugin was
not allowed to say something, and never what it was going to say. And it is
**observed, not reported**: every event originates on the host side of the
boundary at the point the decision is made, so a guest can cause an event and
can neither suppress nor forge one.

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

- **There is no `net` allowlist, and there will not be one at this layer.**
  `net` grants the socket *imports* or denies them; it never names a reachable
  host. `net = "linked"` grants the interfaces while wasmtime-wasi refuses every
  connection, which is what a CPython or JavaScript guest needs. The reason is
  structural: `socket_addr_check` is handed a resolved `SocketAddr` and never
  the hostname that produced it, so watoots cannot enforce a rule about a name,
  and an unenforced allowlist in a manifest is worse than none — it reads as a
  restriction. Hostname policy belongs to whatever your application serves
  behind `wasi:http`, where the name still exists. See
  [ADR-0012](adr/0012-no-net-allowlist.md).
- **Signature verification is pinned-key only, and off unless configured.**
  `[signature]` in the manifest checks that the bytes were signed by a key you
  listed. It does not check identity, certificate chains, or transparency-log
  inclusion — that is Sigstore keyless verification, which needs network access
  and a maintained trust root at load time, and watoots does neither. Verify a
  bundle where you fetch the plugin if you need that. Note also that a manifest
  with no `[signature]` section verifies nothing: unlike every permission, this
  one is off by default, so that upgrading does not break existing plugins.
  Key rotation and revocation are the deployment's problem, not watoots'.
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

See **[SECURITY.md](../SECURITY.md)** in the repository root: the contact
address, the response times you can expect, and the split with upstream
Wasmtime. It lives there because that is where GitHub looks for it.
