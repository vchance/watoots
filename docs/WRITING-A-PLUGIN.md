# Writing your first plugin

Fifteen minutes, in Rust, from nothing to a plugin running under a policy. Every
command here was run in order and its output pasted back; if one does not work
for you that is a bug worth reporting.

You need Rust with the `wasm32-wasip2` target (`rustup target add
wasm32-wasip2`) and a `watoots` binary (`cargo build --release -p watoots-cli`,
or `cargo run -p watoots-cli --`).

## 1. Say what a plugin is

A plugin implements a **world**: the functions it must export, and the host
functions it may import. This is WIT, the component model's interface language,
and it is the contract both sides compile against.

`wit/greet.wit`:

```wit
package example:greet@0.1.0;

world greeter {
  export greet: func(name: string) -> string;
}
```

That is the smallest useful world — one export, no host imports. Nothing here is
watoots-specific; it is the same WIT any component-model tool reads.

## 2. Write the plugin

`Cargo.toml`:

```toml
[workspace]                 # its own workspace: this builds for wasm, not your host

[package]
name = "greeter"
version = "0.0.0"
edition = "2024"

[lib]
crate-type = ["cdylib"]     # required: a component is a dynamic library

[dependencies]
wit-bindgen = "0.61"
```

`src/lib.rs`:

```rust
wit_bindgen::generate!({ path: "wit", world: "greeter" });

struct Greeter;

impl Guest for Greeter {
    fn greet(name: String) -> String {
        format!("hello, {name}")
    }
}

export!(Greeter);
```

`generate!` reads the WIT and produces the `Guest` trait; `export!` wires your
type up as the component's implementation. There is no serialization code to
write on either side, and no agreement about JSON or protobuf — the types in the
world *are* the interface.

## 3. Build it

```sh
cargo build --target wasm32-wasip2 --release
```

That produces `target/wasm32-wasip2/release/greeter.wasm`, about 65 KB. It is a
component already: `wasm32-wasip2` emits one directly, so there is no separate
"componentize" step.

## 4. Find out what it wants

Before writing any policy, ask:

```console
$ watoots inspect greeter.wasm
capabilities
  filesystem   -      not requested, not granted
  network      -      not requested, not granted
  clock        DENY   wanted; no clock granted
  environment  DENY   wanted; cannot read the environment
  random       -      not requested, not granted
  logging      -      not requested, not granted

publisher
  signature    WARN   not verified - any bytes at this path load, with everything granted above

14 import(s): 12 need no grant, 2 not granted

2 import(s) are not granted; `--imports` lists them individually
```

**This is the surprising part, and it is the point.** Your plugin formats a
string. It does not read a clock or an environment variable. But Rust's `std`
links `wasi:clocks/monotonic-clock` and `wasi:cli/environment` whether you use
them or not, so the component *declares* those imports and watoots reports them.

The import list reflects the **toolchain**, not the author. A JavaScript guest
will want the wall clock because its engine needs `Date`; a Python guest will
want sockets because CPython links them at startup. None of that is visible in
anyone's source code, and all of it is visible here — before the plugin runs.

Note the exit code is non-zero, so this works as a CI gate.

## 5. Grant exactly that

`policy.toml`:

```toml
# What this plugin may touch. Everything absent is denied.
[permissions]
clocks = "monotonic"    # rust std links one whether or not you use it
env    = {}             # so does this: it may look, and will find nothing

[signature]
required = false        # a sample built from source, not a signed release
```

Two things worth noticing. `env = {}` is a *grant*, not an absence: the plugin
may read its environment and will find it empty, which is different from not
being allowed to look. And `[signature]` is not optional — a policy file has to
say whether plugins must be signed, so that "nobody thought about it" and "we
decided not to" cannot look the same. Section 7 covers turning it on.

```console
$ watoots inspect greeter.wasm -m policy.toml
...
every import is granted
```

## 6. Run it

```console
$ watoots run greeter.wasm -m policy.toml -c greet -- '"world"'
"hello, world"
```

Arguments and results are [WAVE](https://github.com/bytecodealliance/wasm-tools/tree/main/crates/wasm-wave)
text — the component model's own value syntax — so `"world"` is a string and
`{line: 1, column: 4}` would be a record. You never wrote a parser for either
side.

## 7. Where to go next

- **Give plugins a host function.** Add `import log;` to the world and an
  `interface log { ... }` beside it; the host serves it with
  `Host::builder().host_func(...)`, or `--answer` on the command line for a
  quick run. `examples/wit/lint` is the worked version.
- **Another language.** The same world, in C++, JavaScript and Python, is in
  `examples/plugins/`. The host binary is not recompiled between them.
- **Sign it**, so a replaced file cannot impersonate yours:
  `cosign sign-blob --key cosign.key --output-signature greeter.wasm.sig
  greeter.wasm`, then list the public key under `[signature]` instead of
  `required = false`. See the README's *Who wrote it*.
- **Record a bug.** `watoots record ... -o bug.wave` writes every crossing as
  text; `watoots replay bug.wave -c greeter.wasm --assert` re-runs it with no
  host application present. That is the second half of the project.
- **Before you ship an update**, `watoots diff old.wasm new.wasm -m policy.toml`
  says whether the new build wants anything the policy does not grant — which is
  what a reload would refuse.

Full manifest reference: [MANIFEST.md](MANIFEST.md). What the sandbox does and
does not protect against: [SECURITY.md](SECURITY.md).
