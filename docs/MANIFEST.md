# The manifest

A manifest is a TOML file that says what a plugin may touch and how much of it
may consume. It is the thing watoots exists to enforce.

```toml
[permissions]
fs.read  = ["${plugin_dir}", "${workspace}/**/*.md"]
fs.write = ["${plugin_dir}/cache"]
clocks   = "monotonic"
random   = true

[limits]
memory  = "64MiB"
fuel    = 50_000_000
timeout = "200ms"
```

Everything is denied unless the manifest grants it. There is no way to start
from "allow everything" and subtract.

## What makes this different from a config file

A WebAssembly component **declares its imports in the binary**. You can read
them without running it. So at load time watoots intersects what a plugin asks
for against what you granted, and refuses the load if anything is uncovered:

```
$ watoots inspect lint.wasm -m policy.toml
  ok   wasi:clocks/monotonic-clock@0.2.9  [permissions.clocks = "monotonic"]
  DENY wasi:sockets/tcp@0.2.9             [permissions.net]

1 import(s) are not granted
```

That is a *load* error, not a runtime trap. No guest code has run. You find out
a plugin wants the network when you install it, rather than the first time it
reaches for a socket, and `watoots inspect` exits non-zero so it works in a
gate.

## `[permissions]`

| Key | Type | Grants |
|---|---|---|
| `fs.read` | list of paths | `wasi:filesystem`, preopened read-only |
| `fs.write` | list of paths | `wasi:filesystem`, preopened read-write |
| `net` | list of hosts | `wasi:sockets`, `wasi:http` |
| `env` | table | `wasi:cli/environment` |
| `clocks` | `"monotonic"` \| `"wall"` | `wasi:clocks` |
| `random` | bool | `wasi:random` |
| `logging` | `"trace"` … `"critical"` | `wasi:logging` |

Absent keys deny. Two of them have a third state that matters:

### `net` and `env`: present-but-empty is a grant

```toml
net = []     # the interfaces exist; no host is reachable
env = {}     # the guest may read its environment and find nothing
```

This is not pedantry. A CPython guest links `wasi:sockets` whether or not the
plugin opens a socket, and a JavaScript guest links `wasi:http`. Denying the
*import* would refuse those languages outright. Granting the import while
wasmtime-wasi's own defaults refuse every connection is what actually matches
the situation: the plugin can see the door and cannot open it.

> **`net` with a non-empty list is refused.** Nothing enforces the allowlist
> yet, and a manifest naming one host must not quietly hand over the whole
> network. Use `net = []` until the allowlist lands.

### `clocks` is a ladder

`"wall"` implies `"monotonic"`. They are separate because the wall clock is the
one that makes a recorded trace non-reproducible.

### `logging` names a floor, and is how a plugin says anything at all

```toml
logging = "warn"   # warn, error and critical reach the sink; the rest are dropped
```

Default-deny has an edge that only shows up in production: a plugin with no
filesystem, no sockets and no stdout worth the name has **no way to report that
something is wrong**. The host learns that a call failed and nothing about why.
[`wasi:logging`][wasi-logging] is the standard answer — one function,
`log(level, context, message)` — so a plugin written for wasmCloud or Spin logs
correctly here unchanged.

Absence denies, like everything else: a component importing `wasi:logging` under
a manifest that does not mention it fails to load. A level names the *least
severe* message that reaches the application; `"trace"` admits all six.

Where those messages go is the application's decision, not the manifest's:
`HostBuilder::log_sink` in Rust, `wt_host_builder_log_sink` in C,
`HostBuilder::LogSink` in C++. watoots wires the interface and stays out of the
logging-framework business. A grant with no sink links and discards. The `watoots`
CLI prints to stderr.

Two things the sink does not get, on purpose. There is **no timestamp** — a
guest-supplied one would defeat the pinned wall clock and make a recording
unreplayable, and your logging framework already stamps records from the host
clock at an instant nearer the truth. And there are **no guest-emitted metrics**;
see [ADR-0006](adr/0006-logging-and-metrics.md) for why that is a sandbox
argument rather than a scheduling one.

> Note that `wasi:logging` is a [Phase 1 proposal][proposals] published as
> `0.1.0-draft`, and a prerelease version is semver-compatible with nothing but
> itself. watoots serves both `wasi:logging/logging@0.1.0-draft` and the bare
> `wasi:logging/logging`, so a guest built from a version-stripped copy of the
> WIT also links. Budget for one breaking adjustment before it stabilises.

[wasi-logging]: https://github.com/WebAssembly/wasi-logging
[proposals]: https://github.com/WebAssembly/WASI/blob/main/Proposals.md

### Filesystem grants are directory-granular

A path in `fs.read` is preopened as a directory. WASI preopens are directories,
not glob patterns, so a grant admits the whole tree beneath it. Grant the
narrowest directory that works.

## `[limits]`

| Key | Type | Default | Enforced by |
|---|---|---|---|
| `memory` | `"64MiB"` or bytes | 64 MiB | `StoreLimits` |
| `fuel` | integer | none | fuel metering, per call |
| `timeout` | `"200ms"` | none | epoch interruption, per call |
| `log_bytes` | `"64KiB"` or bytes | 64 KiB | `wasi:logging`, per call |
| `log_messages` | integer | 1024 | `wasi:logging`, per call |

`fuel`, `timeout`, `log_bytes` and `log_messages` are **per call**, not per
plugin lifetime: all four are re-armed before every export invocation. Limits are also per plugin — each owns its
store, so one plugin exhausting its fuel says nothing about its neighbours.

An absent `[limits]` table still caps memory and log volume. "No limits stated"
should not mean "this plugin may exhaust the host".

The two log ceilings exist because fuel does not cover this: fuel bounds how many
times a plugin loops, not how many bytes each iteration pushes into your logging
pipeline, and lifting a guest string into a host string is work the host pays for
outside the fuel budget. `log_bytes` counts the context and the message together
and is charged **before** the `logging` level filters — otherwise
`logging = "critical"` would be a licence to push unbounded bytes across the
boundary at `trace`. Exceeding either ceiling fails the call with
`WT_ERR_LIMIT_EXCEEDED`, the same way running out of fuel does.

## `[determinism]`

| Key | Type | Default |
|---|---|---|
| `enabled` | bool | `true` |
| `epoch_seconds` | integer | `0` |
| `monotonic_step_nanos` | integer | `1000000` |
| `seed` | string | `"watoots"` |

With `enabled`, watoots canonicalises NaNs, makes relaxed-SIMD deterministic,
pins the wall clock, advances the monotonic clock by a fixed step per read, and
seeds both random generators.

On by default, because the characteristic failure of a plugin host is "it
behaved differently on a user's machine and I cannot reproduce it". NaN
canonicalisation costs something on float-heavy guests; turn it off there and
accept that their traces are less portable.

These are manifest keys rather than host-builder options on purpose: **the
manifest travels inside a recorded trace**, so replay rebuilds the same engine
configuration. Otherwise a divergence report could be a statement about the
engine rather than about the plugin.

## Variables

`${name}` in any path or environment value is expanded at load time.

- `${plugin_dir}` is defined automatically as the directory a plugin was loaded
  from, so a grant can name it without knowing it in advance.
- Anything else comes from `HostBuilder::var` / `wt_host_builder_var` /
  `watoots --var`.

An unknown variable is an error. Expanding `${workspace}/**` to `/**` because a
variable was missing would widen a grant rather than narrow it, so it fails
instead.

## The import list reflects the toolchain, not the author

The most common surprise. None of the three sample plugins in `examples/` uses
the capabilities it is granted — all three do string processing and call one
host function — yet their policies differ:

| | Rust | JavaScript | Python |
|---|:-:|:-:|:-:|
| monotonic clock, environment | ✓ | ✓ | ✓ |
| wall clock, filesystem | | ✓ | ✓ |
| random, socket interfaces | | | ✓ |

A Rust guest pulls the clock and environment through `std`. StarlingMonkey needs
the wall clock for `Date` and the filesystem for module resolution. CPython
links sockets and seeds hash randomisation at startup.

So a denial that looks wrong is usually the runtime, not the plugin. Two things
follow: read `watoots inspect` before writing a policy, and remember that build
flags can shrink the bill — `js-lint` is built with ComponentizeJS's
`--disable http --disable random --disable fetch-event`, which removes three
grants that would otherwise be required.

## Interfaces imported only for their types

`watoots:example/log` using `severity` from `watoots:example/types` puts the
whole `types` interface in the import list. It has no callable functions, so it
is not a capability and needs no grant — reported as
`(types only, nothing callable)`.

## Serving your own interfaces

An interface your application implements is not a manifest question. Register it
with `HostBuilder::host_func` (or `wt_host_builder_host_func`), which also
declares it to the grant check so the two cannot drift apart.

Spell the interface as the component imports it, version included
(`watoots:example/log@0.1.0`) — that is what the linker matches on. Grants match
unversioned, so rebuilding a guest against a new patch of an interface does not
invalidate a manifest.
