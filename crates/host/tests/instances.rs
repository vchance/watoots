//! Many instances of one component: the normal shape of a plugin host.

use watoots::{Host, Manifest, Registry, Val};

/// Counts its own calls, so two instances can be told apart by their state.
const COUNTER: &str = r#"
(component
  (core module $m
    (global $n (mut i32) (i32.const 0))
    (func (export "bump") (result i32)
      (global.set $n (i32.add (global.get $n) (i32.const 1)))
      (global.get $n)))
  (core instance $i (instantiate $m))
  (func $bump (result s32) (canon lift (core func $i "bump")))
  (export "bump" (func $bump))
)
"#;

fn host() -> Host {
    Host::builder()
        .manifest(Manifest::parse("[limits]\nfuel = 100_000_000\n").unwrap())
        .build()
        .unwrap()
}

fn bump(plugin: &mut watoots::Plugin) -> i32 {
    match plugin.call("bump", &[]).unwrap().as_slice() {
        [Val::S32(n)] => *n,
        other => panic!("expected one s32, got {other:?}"),
    }
}

#[test]
fn the_same_component_loads_many_times_and_each_instance_owns_its_state() {
    let host = host();
    let mut first = host.load_binary("a", COUNTER.as_bytes()).unwrap();
    let mut second = host.load_binary("b", COUNTER.as_bytes()).unwrap();

    assert_eq!(bump(&mut first), 1);
    assert_eq!(bump(&mut first), 2);
    // A separate store, so a separate global. If instances shared anything
    // this would be 3.
    assert_eq!(bump(&mut second), 1);
    assert_eq!(bump(&mut first), 3);
}

#[test]
fn loading_the_same_bytes_again_does_not_build_them_again() {
    // The point of the in-memory cache. Without it every instance of an
    // interpreter guest pays a full compile, which for the 18MB Python sample
    // is seconds, and nothing in the API hinted at it.
    let host = host();
    assert_eq!(host.compiles(), 0, "nothing built yet");

    let _a = host.load_binary("a", COUNTER.as_bytes()).unwrap();
    assert_eq!(host.compiles(), 1);

    for name in ["b", "c", "d"] {
        let _ = host.load_binary(name, COUNTER.as_bytes()).unwrap();
    }
    assert_eq!(host.compiles(), 1, "same bytes, one build");
}

#[test]
fn different_bytes_still_build() {
    let host = host();
    let _a = host.load_binary("a", COUNTER.as_bytes()).unwrap();
    // A trailing comment changes the bytes and therefore the key.
    let other = format!("{COUNTER}\n;; different\n");
    let _b = host.load_binary("b", other.as_bytes()).unwrap();
    assert_eq!(host.compiles(), 2);
}

#[test]
fn a_registry_refuses_a_duplicate_name_but_not_a_duplicate_component() {
    let mut registry = Registry::new(host());
    registry.load_binary("one", COUNTER.as_bytes()).unwrap();
    // Same component, different name: fine, and the usual way to run two.
    registry.load_binary("two", COUNTER.as_bytes()).unwrap();

    let err = registry
        .load_binary("one", COUNTER.as_bytes())
        .expect_err("silently replacing would lose the first plugin's state");
    assert!(
        err.message().contains("already registered"),
        "{}",
        err.message()
    );
    assert_eq!(registry.len(), 2);
}

#[test]
fn the_compiled_cache_is_bounded_and_forgets_the_oldest_first() {
    // A host that reloads on every file save sees new bytes every time. Without
    // a bound, a morning's editing accumulates every build it ever compiled.
    let host = host();
    let variant = |n: usize| format!("{COUNTER}\n;; {n}\n");

    // One more than the capacity, so exactly the first is evicted.
    let capacity = 32;
    for n in 0..=capacity {
        let _ = host
            .load_binary(&format!("p{n}"), variant(n).as_bytes())
            .unwrap();
    }
    assert_eq!(host.compiles(), capacity as u64 + 1);

    // The newest is still cached: no rebuild.
    let _ = host
        .load_binary("again", variant(capacity).as_bytes())
        .unwrap();
    assert_eq!(host.compiles(), capacity as u64 + 1, "newest was evicted");

    // The oldest was evicted, so it builds again.
    let _ = host
        .load_binary("first-again", variant(0).as_bytes())
        .unwrap();
    assert_eq!(
        host.compiles(),
        capacity as u64 + 2,
        "oldest survived the bound"
    );
}
