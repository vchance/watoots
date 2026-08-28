//! A minimal end-to-end walk through the host library.
//!
//!     cargo run -p watoots --example minimal
//!
//! Components here are written in WAT so the example is self-contained. A real
//! plugin is a `.wasm` built from the WIT world in `examples/wit/lint.wit` and
//! loaded with `host.load("lint.wasm")`.

use watoots::{Host, Manifest, Val};

/// Imports nothing. Exports one function returning 42.
const SELF_CONTAINED: &str = r#"
(component
  (core module $m
    (func (export "answer") (result i32) i32.const 42))
  (core instance $i (instantiate $m))
  (func $answer (result s32) (canon lift (core func $i "answer")))
  (export "answer" (func $answer))
)
"#;

/// Wants to touch the filesystem. Note the version: the grant check ignores it,
/// but instantiation needs the exact name wasmtime-wasi 48 provides.
const WANTS_FILES: &str = r#"
(component
  (import "wasi:filesystem/types@0.2.12" (instance))
)
"#;

/// Never returns on its own.
const RUNAWAY: &str = r#"
(component
  (core module $m
    (func (export "spin") (loop $l br $l)))
  (core instance $i (instantiate $m))
  (func $spin (canon lift (core func $i "spin")))
  (export "spin" (func $spin))
)
"#;

fn main() -> Result<(), watoots::Error> {
    // 1. A host with no grants at all. Everything is denied by default; you opt
    //    in, never out.
    let deny_all = Host::builder().build()?;

    // 2. A component that imports nothing needs no grants, so it loads and runs.
    let mut plugin = deny_all.load_binary("answer", SELF_CONTAINED.as_bytes())?;
    println!("1. call answer()      -> {:?}", plugin.call("answer", &[])?);

    // 3. A component that wants the filesystem is refused at LOAD time. Nothing
    //    is instantiated and no guest code has run.
    match deny_all.load_binary("reader", WANTS_FILES.as_bytes()) {
        Ok(_) => unreachable!("the manifest grants no filesystem access"),
        Err(err) => {
            println!("\n2. loading a plugin that wants files:");
            println!(
                "   {:?}: {}",
                err.kind(),
                err.message().lines().next().unwrap()
            );
        }
    }

    // 4. Ask what it wants, without instantiating it. This is what you would
    //    show a user before they install a plugin.
    println!("\n3. inspect, before installing:");
    print!("{}", deny_all.inspect(WANTS_FILES.as_bytes())?.describe());

    // 5. Grant the directory, and the same component loads.
    let granting = Host::builder()
        .manifest(Manifest::parse(
            r#"
            [permissions]
            fs.read = ["${workspace}/docs"]
            "#,
        )?)
        .var(
            "workspace",
            env!("CARGO_MANIFEST_DIR").to_string() + "/../..",
        )
        .build()?;

    let reader = granting.load_binary("reader", WANTS_FILES.as_bytes())?;
    println!(
        "\n4. same plugin, with fs.read granted -> loaded as {:?}",
        reader.name()
    );

    // 6. Limits are per call. A runaway guest is stopped by whichever ceiling
    //    it hits first; here, fuel.
    let metered = Host::builder()
        .manifest(Manifest::parse(
            "[limits]\nfuel = 100_000\ntimeout = \"100ms\"\n",
        )?)
        .build()?;
    let mut runaway = metered.load_binary("runaway", RUNAWAY.as_bytes())?;

    let err = runaway.call("spin", &[]).unwrap_err();
    println!("\n5. an infinite loop, metered:");
    println!(
        "   {:?}: {}",
        err.kind(),
        err.message().lines().next().unwrap()
    );

    // The plugin is still usable afterwards: the budget is per call.
    println!("\n6. plugin survives; budgets are per call, not per lifetime");
    let _ = Val::Bool(true);
    Ok(())
}
