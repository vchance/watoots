# watoots — scoping spec

*Verified 2026-08-27 against source; engine baseline Wasmtime 48 LTS; target WASI 0.2.x, designed for 0.3.*

A batteries-included plugin host for native applications on the WebAssembly component model, plus a WIT-level record/replay tool. This document is the source of truth for scope. `scoping.html` is the same content as the published scoping page.

## Verdict

- **The gap is real and unoccupied.** Extism (5.7k stars, 15 host SDKs) is the only general "plugins for your app" library, and it still has no component-model support after three years. Every component-model plugin system (Zed, Veloren, wasvy, Spin, wasmCloud) hand-rolled its own host layer in Rust.
- **Build "Extism for components":** manifest-driven permissions, resource limits, precompile cache, typed host APIs, lifecycle — with a C API on day one so C++ hosts are first-class. That last part is what nobody on the component side offers.
- **Record/replay is more open than it first looked.** Wasmtime started an engine-level, binary RR (two PRs merged Feb 2026), but the third PR has been idle since April and the `wasmtime replay` command its flags reference does not exist in the tree. Ours sits above it anyway — WIT-typed, human-readable, host-free replay that turns a bug report into a regression test — and may be the only usable component replay story for a while.
- **Drop the WIT compatibility checker** — `wasm-tools component semver-check` already exists. Keep the capability auditor and the WIT-driven fuzzer as follow-ons; neither has a standalone tool.

## Landscape

Bytes-ABI systems are polyglot but untyped; component-model systems are typed but Rust-only in practice, and each one rebuilt the host layer from scratch.

| System | Engine | Components / WIT | Permissions model | Hot reload |
|---|---|---|---|---|
| Extism 1.30 | Wasmtime 43 (Rust); wazero (Go) | No — bytes in/out, WASI p1 | Manifest: `allowed_hosts`, `allowed_paths`, memory, fuel, timeout | Reinstantiate |
| Zed | Wasmtime 48 | Yes — versioned WIT, wasip2 | Hardcoded: RW preopen of per-extension dir, epoch interruption; no user prompts | Uninstall/reinstall |
| Lapce 0.4.6 | Wasmtime 14 (pinned) | No — WASI p1 core modules | WASI preopens | unknown |
| Spin 4.1 | Wasmtime 44 | Yes — wasip2, p3 HTTP middleware | WASI + `allowed_outbound_hosts` | `spin watch` rebuild |
| wasmCloud 2.5 | Wasmtime 46 | Yes — WASI p3 default | Capability providers via links; `implements` multiplexing | Restart only (proposed) |
| Wassette (Microsoft) | Wasmtime | Yes — components as MCP tools | Deny-by-default permission prompts | unknown |
| Veloren | Wasmtime 45 | Yes — WASI 0.2 WIT | Sandboxed, shipped to clients | unknown |
| wasvy (Bevy) 0.0.9 | Wasmtime | Yes — WIT for ECS | "Mod permissions" deferred to phase 2 | Rebuild via `wasvy dev` |
| Shopify Functions | Fuel-metered, undocumented | No — core modules | Hard limits: 11M instr, 10 MB, 5 ms | n/a |
| Envoy proxy-wasm | V8 / WAMR / Wasmtime | No — proxy-wasm ABI 0.2.1 | ABI host calls only | xDS config swap |
| SpacetimeDB 2.8 | Wasmtime + V8 | No — custom C ABI | Energy budget → tx rollback | Republish |

Adoption signals: Helm 4 chose Extism explicitly because wazero lacks components and listed "extra permissions" as an open need (HIP-0026); wazero closed component support as *not planned* in May 2026, locking Go hosts out of the component ecosystem; the runtime-agnostic `wasm_component_layer` was abandoned and its fork `waclay` is at 0.2.

## Engine readiness

What Wasmtime 48 already gives us, so we build on it rather than around it.

- **Component model is Tier 1** on x86_64/aarch64 for both Cranelift and Winch. `bindgen!` supports async, tracing, trappable errors, resource mapping, and (48.0) emits `COMPONENT_TYPE` for reflection. 48.x is the next LTS line: 24 months of patches.
- **Limits are mature:** fuel with per-opcode costs (48.0), epoch interruption (~10% overhead, async-yield capable), `StoreLimits`/`ResourceLimiter`, pooling allocator with memory/instance caps.
- **Precompilation:** `Engine::precompile_component` → `.cwasm`, plus a built-in compile cache. Wizer now lives in-tree for init-time snapshots.
- **No snapshot/restore, and nobody is working on it:** issue #4002 has been open since April 2022 with no activity since. The v0.2 checkpoint experiment isn't racing anyone.
- **Debug/profile hooks exist:** gdbstub guest debugger (`wasmtime -g`, since 44), core dumps on trap, `GuestProfiler` emitting Firefox-profiler JSON. DWARF source debugging is officially "best-effort, no maintainer" — we don't touch it.
- **WASI:** 0.2.12 is the stable, universally-supported target. 0.3.0 was ratified 2026-06-11 (async, `stream<T>`/`future<T>`, `wasi:io` deleted), but `wasmtime-wasi::p3` is documented as not production-ready and `wasm32-wasip3` is Rust Tier 3. Ship on 0.2, keep the permission model 0.3-shaped.
- **48.0 defaults changed in our favor:** `wasmtime-wasi` now denies TCP/UDP by default and simplified filesystem perms to ro/rw — the same shape a manifest wants.
- **Guest languages via wit-bindgen 0.61:** Rust, C, C++, C#, Go, MoonBit, D in-tree; JS via ComponentizeJS 0.22; Python via componentize-py 0.25. Enough to demo polyglot plugins honestly.

## Gaps → features

| Ecosystem gap | Evidence | Our answer |
|---|---|---|
| No batteries-included component host; every app rebuilds it | Zed, Veloren, wasvy, Spin each own a bespoke `wasm_host.rs` | Host library: engine config presets, manifest, WASI wiring, lifecycle, plugin registry |
| Permissions UX is absent or hardcoded | Zed hardcodes a work dir; wasvy defers; Helm HIP-0026 lists it as open; only Extism and Wassette have grants | Manifest permissions + `inspect` that explains a component's imports as a human-readable grant list |
| C/C++ hosts have no component-model option | Extism is the only polyglot host and it's bytes-ABI; component embedders are Rust-only | C API (cbindgen) + C++ header from v0.1 |
| Host API boilerplate | wasmtime #9294, #11287, #8857, #9600; "first-class functions in WIT are the gap for SDK generation" | Dynamic typed calls (`Val` + WAVE) alongside `bindgen!`; reentrancy patterns documented, not hidden |
| Debugging plugins is painful | Wasmtime stability tiers; LLDB step bug #12995; "still painful" (Apr 2026) | Record/replay at the WIT level: reproduce a plugin bug without the host |
| Hot reload loses state | wasmtime #4002 open since 2022, no activity since; every app reinstantiates | v0.2: reload with optional `export-state`/`import-state` hook; same-binary checkpoint via memory/global/table copy (feasible, unproven) |
| No typed fuzzing of components | Only Wasmtime's internal `component_api` fuzz oracle; no standalone tool | Follow-on: WIT-driven fuzzer that emits replay files for each crash |

### What we deliberately don't build

- **WIT compatibility checker** — `wasm-tools component semver-check` covers it. Wrap it in `inspect`, don't reimplement.
- **Engine-level deterministic replay** — Wasmtime's `rr` feature is the right home for that (binary trace, ~4–5% overhead, human-readable explicitly a non-goal). It is currently stalled: `RRConfig::{Recording, Replaying}` and the `rr` cargo feature exist, but replay is unimplemented and the last PR has sat since April. We complement it; if it ships, it becomes an optional bit-exact backend.
- **Source-level debugging** — DWARF in Wasmtime is unmaintained; the new gdbstub debugger is the path, and it's theirs.
- **Cross-version state migration** (Erlang-style hot code loading) — too big for the first year. Same-binary reload with state hooks is the honest v0.2.

## The two pieces

### 1. The host library

A Rust crate plus a C API that turns "I have a Wasmtime dependency" into "I have a plugin system." An application declares a WIT world for its plugin interface, then:

```rust
// Rust host
let host = Host::builder()
    .manifest_from_file("plugins/policy.toml")?   // permissions + limits
    .cache_dir(cache)?                              // .cwasm precompile cache
    .build()?;

let plugin = host.load("plugins/lint.wasm")?;       // validated against the world
let out: LintResult = plugin.call("lint", &input)?; // typed via bindgen or WAVE
```

```toml
# policy.toml — what the plugin may touch
[permissions]
fs.read  = ["${plugin_dir}", "${workspace}/**/*.md"]
fs.write = ["${plugin_dir}/cache"]
net      = []                 # denied (also Wasmtime 48's default)
clocks   = "monotonic"        # no wall clock
random   = true

[limits]
memory   = "64MiB"
fuel     = 50_000_000         # per call
timeout  = "200ms"            # epoch deadline
```

The manifest is the product. It's what Extism users already understand, what Zed hardcoded, what Helm asked for, and what the component model makes *verifiable*: at load time we intersect the component's declared imports against the grants and refuse anything not covered — no runtime surprise.

### 2. Record/replay

Every import and export crossing goes through the host library's linker anyway. Recording is a shim that serializes each crossing as a WIT-typed event (WAVE text for readability, a compact binary for size). Replay instantiates the component under a *mock host* that answers imports from the trace and flags the first divergence.

```
RECORD   Application ⇄ Host library (recording shim in linker) ⇄ Wasmtime component
                                  │ writes every crossing
                                  ▼
                             trace.wave  (WIT-typed events)
                                  │ reads
                                  ▼
REPLAY   (no application)   Mock host (answers imports from trace) ⇄ same component
                                  │
                                  ▼
                 first divergence → report, or pass → regression test
```

Recording is a linker shim in the host library; replay swaps the application for a mock host that serves the same trace. The application never has to be present to reproduce a plugin bug.

Why this is different from Wasmtime's own `rr`: theirs records at the canonical-ABI lowering level for bit-exact engine determinism, in a binary format, and cannot be read or edited. Ours records at the WIT level, is readable and diffable, and a trace is a test fixture: `watoots replay trace.wave --assert` is a regression test with no host code. If Wasmtime's replay ever lands, the two compose — theirs for "reproduce exactly", ours for "explain and keep". As of today theirs has recording knobs and no replay, so for components this would be the first working replay of any kind.

## MVP scope

### Host library v0.1

**In**
- Load component from bytes/file; validate against the host's WIT world (`wasm-tools component targets` logic)
- Manifest (TOML): fs ro/rw globs, net allowlist, env, clocks, random; memory/fuel/epoch limits
- Import-intersection check at load: undeclared imports are a load error, not a runtime trap
- WASI 0.2 wiring driven by the manifest
- Typed calls two ways: re-exported `bindgen!` for Rust hosts; dynamic `Val` + WAVE for C and CLI
- Precompile cache keyed on engine config hash + component hash
- Plugin registry: many plugins, one engine, per-plugin store and limits
- C API via cbindgen + C++ RAII header; CMake package
- Host-call trace hook (the seam record/replay plugs into)
- Three sample plugins: Rust, JS (ComponentizeJS), Python (componentize-py) against one WIT world

**Out (v0.1)**
- WASI 0.3 / async component calls
- Hot reload with state
- Permission prompts UI (we emit the grant list; the app renders it)
- Host SDKs beyond Rust and C/C++
- Cross-component composition (`wac` is fine for that)
- Wasmer / WAMR backends

### Record/replay v0.1

**In**
- Trace format: header (component hash, world, engine config), then ordered WIT-typed events for every import/export crossing, with resource handles mapped to stable IDs
- WAVE text encoding + a length-prefixed binary encoding; `watoots trace fmt` converts
- Recorder: shim installed by the host library when `record = true`
- Replay runner: mock host from trace, drives exports, compares each import call (args) to the trace; first divergence → structured diff
- Determinism knobs applied on record and replay: NaN canonicalization, deterministic relaxed-SIMD, manifest-pinned clock/random
- `watoots replay --assert` exit code for CI; `--emit-test` writes a Rust test fixture

**Out (v0.1)**
- Recording guest memory or engine state (that's Wasmtime `rr`'s job)
- Async/stream imports (WASI 0.3)
- Trace minimization / shrinking
- Multi-plugin cross-talk traces

### v0.2 candidates

- `watoots inspect plugin.wasm`: human-readable permission manifest from imports ("reads files under X, no network, uses monotonic clock"), plus `semver-check` and `targets` wrapped.
- Reload: drop/reinstantiate with optional `export-state`/`import-state` WIT hooks; experiment with same-binary checkpoint by copying linear memory, globals, and tables through the public API.
- WIT-driven fuzzer reusing Wasmtime's `component_api` oracle pattern (or `mutatis`), emitting a replay trace per crash.
- Profiler view: wrap `GuestProfiler` and attribute time to guest vs. host-call vs. boundary marshalling per WIT function.

## Sequencing

Evenings-and-weekends pace, one focused person. Ordered by dependency: the trace hook in M2 is what M4 builds on; the C API in M3 is what makes M5's demo credible to C++ audiences.

| Milestone | When | Deliverable |
|---|---|---|
| **M1 — Spike and shape** | Weeks 1–2 | Wasmtime 48 embedding with one WIT world, manifest parser, import-intersection check, fuel/epoch/memory limits. Decide the crate layout and the name. |
| **M2 — Host library core** | Weeks 3–4 | Registry, precompile cache, WASI 0.2 wiring from manifest, dynamic `Val`/WAVE calls, trace hook. Rust sample plugin end to end. |
| **M3 — C API and polyglot proof** | Weeks 5–6 | cbindgen surface, C++ header, CMake package, a tiny C++ host app. JS and Python sample plugins. **First publishable state.** |
| **M4 — Record/replay** | Weeks 7–9 | Trace format, recorder shim, mock host, divergence diff, `--assert` and `--emit-test`. Dogfood by recording the sample plugins. |
| **M5 — Ship v0.1** | Weeks 10–11 | Docs with the manifest as the front page, a 90-second demo (load a plugin, deny a permission, record a bug, replay it in CI), announce on Bytecode Alliance Zulip and r/rust. Ask Zed/wasvy/Veloren maintainers for a review. |
| **M6 — v0.2 from feedback** | Week 12+ | Pick between `inspect`, reload, and fuzzer based on what the first users actually ask for. |

## Risks

- **Overlap · low — Wasmtime ships engine-level replay.** Downgraded after checking the tree: replay is unimplemented and the effort has been idle since April 2026. Even if it revives, their RR is deliberately binary and non-readable, so the WIT-level, test-fixture positioning survives. Mitigation: design our trace so an engine RR blob can be attached as an optional exact-reproduction payload.
- **Overlap — Extism adds component support.** They've declined for three years and their Go SDK's engine has closed the door. If they do, a Wasmtime-only, component-native toolkit still wins on typed APIs and C++ ergonomics. Mitigation: ship before year end; be the reference people cite.
- **Churn — Wasmtime releases monthly; WASI 0.3 is landing.** Pin to the 48 LTS line (24 months of patches). Keep the manifest schema 0.3-shaped (per-interface grants, no `wasi:io` assumptions) so the upgrade is additive.
- **Adoption — existing component hosts won't migrate.** Zed and Veloren have sunk cost. The audience is the *next* app — and every C++ app, which has no option today. Mitigation: the C++ host demo is not optional.
- **Security — the sandbox is the promise, and Wasmtime still patches escapes.** A filesystem escape and a p3 heap DoS were fixed 2026-08-20. Mitigation: track advisories, default-deny everything, ship a "what this does and doesn't protect against" page from day one.
- **Scope — record/replay resources and reentrancy are subtle.** Resource handles and callbacks (wasmtime #9600) complicate a linear trace. Mitigation: v0.1 supports the WIT subset without host-to-guest reentrancy; document the limit rather than half-support it.

## Where the value actually is

Decomposed honestly (from the 2026-08-27 review):

| Piece | Nature | Value |
|---|---|---|
| Engine config, WASI wiring, precompile cache, registry | Glue | Low |
| C API + C++ header | Glue, but nobody has done it | Medium — value is that it exists |
| Manifest permission schema + import-intersection check | Design decision | High if it becomes the convention |
| WIT-level trace format, resource-handle mapping, divergence semantics, reentrancy handling | Real engineering | High — non-obvious; Wasmtime's team chose a different level |
| Same-binary checkpoint/restore (v0.2) | Real engineering | High — #4002 unsolved since 2022 |
| Tracking Wasmtime releases + advisories for 2 years | Commitment | High — this is what people actually adopt |

Nothing technically stops incumbents from building this; what stops them is incentive (Zed built a host for Zed; Extism has a strategic stance against components). What is defensible is trust, convention, and judgment about what not to build. **Lead with record/replay** — it is where understanding shows and where no incumbent is working — and treat the host library as the delivery vehicle.

## Open decisions

Each becomes an ADR in `docs/adr/` when made.

- ~~**Name.**~~ Decided 2026-08-28: **watoots** — see `adr/0001-name.md`.
- ~~**C API on day one, or Rust-first?**~~ Decided: day one, shipped in M3. `crates/host-capi` (cbindgen), `watoots.hpp`, an installable CMake package, and a C++ host app running Rust, JavaScript and Python plugins over one WIT world. Driving cargo from CMake is `adr/0005-cargo-from-cmake.md`.
- ~~**License.**~~ Decided 2026-08-28: Apache-2.0 WITH LLVM-exception — see `adr/0002-license.md`.
- ~~**Host-side dynamic typing.**~~ Decided 2026-08-28: WAVE, via wasmtime's own implementation — see `adr/0004-wave-and-dynamic-typing.md`. It does **not** handle resources (wasmtime maps them to `Unsupported`), so traces carry resource handles as stable IDs beside the WAVE text rather than inside it.
- **Driving cargo from CMake.** Corrosion, a custom command, or building the
  staticlib out of band? Unresolved; the CMake tree is header-only until M3
  needs it (see `adr/0003-cpp-toolchain.md`).
- **Validation targets.** Which two maintainers to ask for early review — wasvy (actively wants a permission model) and Helm's plugin team (wrote down the permission need) are the warmest.

## Verified against source (2026-08-27)

The claims the plan turns on were checked directly against GitHub, separately from the web-research pass. Everything else in the landscape table came from release notes and community pages and is not load-bearing.

| Claim | Checked | Result |
|---|---|---|
| Wasmtime latest is 48.0.1 | Releases API | Confirmed — v48.0.1, 2026-08-24 |
| Zed builds on Wasmtime 48 | `zed/Cargo.toml` on main | Confirmed — `wasmtime = "48"` |
| Snapshot/restore unsolved | wasmtime #4002 | Confirmed — open; last activity April 2022 |
| Wasmtime RR: recording landed, replay not shipped | PRs #12375, #12576, #12981; `config.rs`; `src/commands/` | Confirmed — two PRs merged Feb 2026; #12981 open, idle since 2026-04-16; `RRConfig` exists; no `replay` command in the tree despite CLI flags referencing it |
| Extism has no component-model support | Releases; issue search | Confirmed — v1.30.0 (2026-06-04); only related open issue is #666 "WASI p2 *without* components", idle since Sep 2025 |
| wazero: components not planned | wazero #2200 | Confirmed — closed `not_planned`, 2026-05-08 |
| WASI 0.3.x is current | Releases API | Confirmed — v0.3.1, 2026-08-11 |
| wasmCloud / Spin / Veloren / wasvy versions; meeting quotes | — | Web read only; not decision-critical |

## Sources

- Wasmtime: [releases](https://github.com/bytecodealliance/wasmtime/releases) · [LTS policy](https://bytecodealliance.org/articles/wasmtime-lts) · [stability tiers](https://docs.wasmtime.dev/stability-tiers.html) · [bindgen! docs](https://docs.rs/wasmtime/latest/wasmtime/component/macro.bindgen.html) · [debugging RFC](https://github.com/bytecodealliance/rfcs/blob/main/accepted/wasmtime-debugging.md) · [debugging docs](https://docs.wasmtime.dev/examples-debugging.html) · [profiling docs](https://docs.wasmtime.dev/examples-profiling.html)
- Wasmtime RR: [#12375](https://github.com/bytecodealliance/wasmtime/pull/12375) · [#12576](https://github.com/bytecodealliance/wasmtime/pull/12576) · [#12981](https://github.com/bytecodealliance/wasmtime/pull/12981) · [prototype repo](https://github.com/bytecodealliance/wasmtime-rr-prototyping)
- Wasmtime issues: [#4002 snapshot/restore](https://github.com/bytecodealliance/wasmtime/issues/4002) · [#9294](https://github.com/bytecodealliance/wasmtime/issues/9294) · [#11287](https://github.com/bytecodealliance/wasmtime/issues/11287) · [#8857](https://github.com/bytecodealliance/wasmtime/issues/8857) · [#9600](https://github.com/bytecodealliance/wasmtime/issues/9600) · [#12995](https://github.com/bytecodealliance/wasmtime/issues/12995)
- WASI: [0.3](https://wasi.dev/releases/wasi-p3) · [roadmap](https://wasi.dev/roadmap) · [wasm32-wasip3 platform support](https://doc.rust-lang.org/rustc/platform-support/wasm32-wasip3.html) · [Rust 2026 goal](https://rust-lang.github.io/goals/2026/wasm-components.html)
- Tooling: [wit-bindgen](https://github.com/bytecodealliance/wit-bindgen) · [wasm-tools](https://github.com/bytecodealliance/wasm-tools) · [WASI-Virt](https://github.com/bytecodealliance/WASI-Virt)
- Extism: [repo](https://github.com/extism/extism) · [component model stance](https://github.com/extism/extism/discussions/334) · [WASI p2 issue #666](https://github.com/extism/extism/issues/666) · [manifest](https://extism.org/docs/concepts/manifest/) · [wazero #2200](https://github.com/wazero/wazero/issues/2200) · [Helm HIP-0026](https://helm.sh/community/hips/hip-0026/)
- Apps: [Zed wasm_host.rs](https://github.com/zed-industries/zed/blob/main/crates/extension_host/src/wasm_host.rs) · [Zed extension docs](https://zed.dev/docs/extensions/developing-extensions) · [wasvy](https://github.com/wasvy-org/wasvy) · [Spin releases](https://github.com/spinframework/spin/releases) · [wasmCloud 2.5](https://wasmcloud.com/community/2026-07-01-community-meeting/) · [Wassette](https://opensource.microsoft.com/blog/2025/08/06/introducing-wassette-webassembly-based-tools-for-ai-agents/)
- Commentary: [wasmCloud on SDK gaps and shared memory](https://wasmcloud.com/community/2025-09-10-community-meeting/) · ["Three years of almost ready" (Apr 2026)](https://www.javacodegeeks.com/2026/04/webassembly-in-2026-three-years-of-almost-ready.html) · [runtime comparison 2026](https://00f.net/2026/06/23/webassembly-runtimes-2026/)
- Prior art: [wasm-r3](https://github.com/sola-st/wasm-r3) · [Wasabi](https://github.com/danleh/wasabi)
