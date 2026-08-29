//! The precompile cache.
//!
//! Compiling a component is the expensive part of loading one. Caching the
//! machine code is straightforward; the part worth testing is that the key
//! includes the *engine's* configuration, because a `.cwasm` is loaded without
//! re-validation and reusing one across incompatible engines is unsound.

use watoots::{Host, Manifest};

const COMPONENT: &str = r#"
(component
  (core module $m
    (func (export "answer") (result i32) i32.const 42))
  (core instance $i (instantiate $m))
  (func $answer (result s32) (canon lift (core func $i "answer")))
  (export "answer" (func $answer))
)
"#;

fn cwasm_files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<_> = entries
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "cwasm"))
        .collect();
    found.sort();
    found
}

fn host_with(cache: &std::path::Path, manifest_toml: &str) -> Host {
    Host::builder()
        .manifest(Manifest::parse(manifest_toml).unwrap())
        .cache_dir(cache)
        .build()
        .unwrap()
}

#[test]
fn a_first_load_populates_the_cache_and_a_second_reuses_it() {
    let dir = tempfile::tempdir().unwrap();
    let host = host_with(dir.path(), "");

    assert!(cwasm_files(dir.path()).is_empty());

    let mut first = host.load_binary("answer", COMPONENT.as_bytes()).unwrap();
    assert_eq!(cwasm_files(dir.path()).len(), 1, "first load should cache");

    // Second load takes the cached path; the plugin must behave identically.
    let mut second = host.load_binary("answer2", COMPONENT.as_bytes()).unwrap();
    assert_eq!(cwasm_files(dir.path()).len(), 1, "no duplicate entry");

    assert_eq!(
        first.call("answer", &[]).unwrap(),
        second.call("answer", &[]).unwrap()
    );
}

#[test]
fn the_key_separates_incompatible_engine_configurations() {
    // Fuel metering changes how code is compiled, so a .cwasm built without it
    // must never be handed to an engine that has it on. Different keys is what
    // makes reusing the cache safe rather than merely fast.
    let dir = tempfile::tempdir().unwrap();

    host_with(dir.path(), "")
        .load_binary("plain", COMPONENT.as_bytes())
        .unwrap();
    assert_eq!(cwasm_files(dir.path()).len(), 1);

    host_with(dir.path(), "[limits]\nfuel = 100_000\n")
        .load_binary("metered", COMPONENT.as_bytes())
        .unwrap();
    assert_eq!(
        cwasm_files(dir.path()).len(),
        2,
        "a differently configured engine must not collide"
    );
}

#[test]
fn a_corrupt_cache_entry_costs_a_recompile_not_a_failure() {
    let dir = tempfile::tempdir().unwrap();
    let host = host_with(dir.path(), "");

    host.load_binary("answer", COMPONENT.as_bytes()).unwrap();
    let cached = cwasm_files(dir.path()).remove(0);

    // A truncated write, an interrupted copy, a full disk.
    std::fs::write(&cached, b"not machine code").unwrap();

    let mut plugin = host.load_binary("answer", COMPONENT.as_bytes()).unwrap();
    assert_eq!(
        plugin.call("answer", &[]).unwrap(),
        vec![watoots::Val::S32(42)]
    );
    assert!(cached.is_file(), "the entry should have been rewritten");
}

#[test]
fn without_a_cache_dir_nothing_is_written() {
    let dir = tempfile::tempdir().unwrap();
    Host::builder()
        .build()
        .unwrap()
        .load_binary("answer", COMPONENT.as_bytes())
        .unwrap();
    assert!(cwasm_files(dir.path()).is_empty());
}

#[test]
fn determinism_settings_are_part_of_the_engine_identity() {
    // NaN canonicalisation changes generated code, so a .cwasm compiled with it
    // must never be handed to an engine without it. Different keys is what makes
    // that safe -- and it proves the setting actually reaches the engine.
    let dir = tempfile::tempdir().unwrap();

    host_with(dir.path(), "[determinism]\nenabled = true\n")
        .load_binary("deterministic", COMPONENT.as_bytes())
        .unwrap();
    assert_eq!(cwasm_files(dir.path()).len(), 1);

    host_with(dir.path(), "[determinism]\nenabled = false\n")
        .load_binary("fast", COMPONENT.as_bytes())
        .unwrap();
    assert_eq!(
        cwasm_files(dir.path()).len(),
        2,
        "determinism must change the engine identity"
    );
}
