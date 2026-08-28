//! Generate `include/watoots.h` from the Rust source.
//!
//! The header is committed rather than generated at consume time: a C or C++
//! project installing the CMake package links a prebuilt library and must never
//! need a Rust toolchain. Writing it here keeps it from going stale — a CI
//! `git diff --exit-code` catches a forgotten regeneration.

use std::path::PathBuf;

fn main() {
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    println!("cargo:rerun-if-changed=src/lib.rs");
    println!("cargo:rerun-if-changed=cbindgen.toml");

    let Ok(bindings) = cbindgen::generate(&crate_dir) else {
        // A syntax error in lib.rs will fail the real compile with a better
        // message than anything we could print here.
        println!("cargo:warning=cbindgen could not parse the crate; header not regenerated");
        return;
    };

    let mut generated = Vec::new();
    bindings.write(&mut generated);

    // Only touch the file when it actually changes, so a build does not
    // needlessly dirty the working tree or the file's mtime.
    let header = crate_dir.join("include/watoots.h");
    let current = std::fs::read(&header).unwrap_or_default();
    if current != generated {
        std::fs::write(&header, &generated).expect("writing include/watoots.h");
    }
}
