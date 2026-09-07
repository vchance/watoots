//! `Plugin::reload`: new bytes, the same manifest, and the old instance still
//! running when anything goes wrong. See `docs/adr/0010-reload.md`.
//!
//! The components are WAT, so the suite needs no guest toolchain. State lives
//! in a mutable core global, which is the smallest thing that both survives a
//! call and is destroyed by reinstantiation — exactly the thing the `save-state`
//! / `restore-state` pair exists to carry across.

use watoots::{
    ErrorKind, Host, Manifest, PluginStats, RESTORE_STATE_EXPORT, Registry, SAVE_STATE_EXPORT, Val,
};

/// A counter with both state hooks. `get` returns it unchanged, which is what
/// makes the replacement below distinguishable.
const COUNTER_V1: &str = r#"
(component
  (core module $m
    (global $n (mut i32) (i32.const 0))
    (func (export "get") (result i32) (global.get $n))
    (func (export "bump") (param i32)
      (global.set $n (i32.add (global.get $n) (local.get 0))))
    (func (export "save") (result i32) (global.get $n))
    (func (export "restore") (param i32) (global.set $n (local.get 0))))
  (core instance $i (instantiate $m))
  (func $get (result u32) (canon lift (core func $i "get")))
  (func $bump (param "n" u32) (canon lift (core func $i "bump")))
  (func $save (result u32) (canon lift (core func $i "save")))
  (func $restore (param "state" u32) (canon lift (core func $i "restore")))
  (export "get" (func $get))
  (export "bump" (func $bump))
  (export "save-state" (func $save))
  (export "restore-state" (func $restore))
)
"#;

/// The same world, new code: `get` adds a thousand, so a single call says both
/// which build is running *and* what state it started from.
const COUNTER_V2: &str = r#"
(component
  (core module $m
    (global $n (mut i32) (i32.const 0))
    (func (export "get") (result i32) (i32.add (global.get $n) (i32.const 1000)))
    (func (export "bump") (param i32)
      (global.set $n (i32.add (global.get $n) (local.get 0))))
    (func (export "save") (result i32) (global.get $n))
    (func (export "restore") (param i32) (global.set $n (local.get 0))))
  (core instance $i (instantiate $m))
  (func $get (result u32) (canon lift (core func $i "get")))
  (func $bump (param "n" u32) (canon lift (core func $i "bump")))
  (func $save (result u32) (canon lift (core func $i "save")))
  (func $restore (param "state" u32) (canon lift (core func $i "restore")))
  (export "get" (func $get))
  (export "bump" (func $bump))
  (export "save-state" (func $save))
  (export "restore-state" (func $restore))
)
"#;

/// The same exports minus the hooks: a world that never asked to survive a
/// reload, and a build that a rollback might legitimately produce.
const COUNTER_NO_HOOKS: &str = r#"
(component
  (core module $m
    (global $n (mut i32) (i32.const 0))
    (func (export "get") (result i32) (i32.add (global.get $n) (i32.const 500)))
    (func (export "bump") (param i32)
      (global.set $n (i32.add (global.get $n) (local.get 0)))))
  (core instance $i (instantiate $m))
  (func $get (result u32) (canon lift (core func $i "get")))
  (func $bump (param "n" u32) (canon lift (core func $i "bump")))
  (export "get" (func $get))
  (export "bump" (func $bump))
)
"#;

/// New code that also wants the network. The whole point of re-running the
/// check: this must not become loadable by arriving as an update.
const COUNTER_WANTS_NET: &str = r#"
(component
  (import "wasi:sockets/tcp@0.2.6" (instance (export "connect" (func))))
  (core module $m
    (global $n (mut i32) (i32.const 0))
    (func (export "get") (result i32) (global.get $n))
    (func (export "bump") (param i32)
      (global.set $n (i32.add (global.get $n) (local.get 0))))
    (func (export "save") (result i32) (global.get $n))
    (func (export "restore") (param i32) (global.set $n (local.get 0))))
  (core instance $i (instantiate $m))
  (func $get (result u32) (canon lift (core func $i "get")))
  (func $bump (param "n" u32) (canon lift (core func $i "bump")))
  (func $save (result u32) (canon lift (core func $i "save")))
  (func $restore (param "state" u32) (canon lift (core func $i "restore")))
  (export "get" (func $get))
  (export "bump" (func $bump))
  (export "save-state" (func $save))
  (export "restore-state" (func $restore))
)
"#;

/// `restore-state` takes a string where the outgoing build hands over a u32.
const COUNTER_WRONG_STATE_TYPE: &str = r#"
(component
  (core module $m
    (memory (export "memory") 1)
    (func (export "realloc") (param i32 i32 i32 i32) (result i32) (i32.const 1024))
    (global $n (mut i32) (i32.const 0))
    (func (export "get") (result i32) (global.get $n))
    (func (export "save") (result i32) (global.get $n))
    (func (export "restore") (param i32 i32) (global.set $n (local.get 1))))
  (core instance $i (instantiate $m))
  (func $get (result u32) (canon lift (core func $i "get")))
  (func $save (result u32) (canon lift (core func $i "save")))
  (func $restore (param "state" string)
    (canon lift (core func $i "restore")
      (memory $i "memory")
      (realloc (func $i "realloc"))
      string-encoding=utf8))
  (export "get" (func $get))
  (export "save-state" (func $save))
  (export "restore-state" (func $restore))
)
"#;

/// `save-state` that takes an argument — a hook the host cannot call.
const COUNTER_BAD_HOOK_SHAPE: &str = r#"
(component
  (core module $m
    (global $n (mut i32) (i32.const 0))
    (func (export "get") (result i32) (global.get $n))
    (func (export "save") (param i32) (result i32) (global.get $n)))
  (core instance $i (instantiate $m))
  (func $get (result u32) (canon lift (core func $i "get")))
  (func $save (param "why" u32) (result u32) (canon lift (core func $i "save")))
  (export "get" (func $get))
  (export "save-state" (func $save))
)
"#;

/// Takes state and traps on the way in, to check the *old* instance is
/// untouched when the replacement is the thing that fails.
const COUNTER_RESTORE_TRAPS: &str = r#"
(component
  (core module $m
    (global $n (mut i32) (i32.const 0))
    (func (export "get") (result i32) (global.get $n))
    (func (export "save") (result i32) (global.get $n))
    (func (export "restore") (param i32) unreachable))
  (core instance $i (instantiate $m))
  (func $get (result u32) (canon lift (core func $i "get")))
  (func $save (result u32) (canon lift (core func $i "save")))
  (func $restore (param "state" u32) (canon lift (core func $i "restore")))
  (export "get" (func $get))
  (export "save-state" (func $save))
  (export "restore-state" (func $restore))
)
"#;

/// `save-state` handing back 60 000 bytes, to be met by a tiny
/// `limits.transfer`.
const COUNTER_HUGE_STATE: &str = r#"
(component
  (core module $m
    (memory (export "memory") 1)
    (func (export "realloc") (param i32 i32 i32 i32) (result i32) (i32.const 61000))
    (func (export "get") (result i32) (i32.const 7))
    (func (export "save") (result i32)
      (i32.store (i32.const 60008) (i32.const 0))
      (i32.store (i32.const 60012) (i32.const 60000))
      (i32.const 60008)))
  (core instance $i (instantiate $m))
  (func $get (result u32) (canon lift (core func $i "get")))
  (func $save (result string)
    (canon lift (core func $i "save")
      (memory $i "memory")
      (realloc (func $i "realloc"))
      string-encoding=utf8))
  (export "get" (func $get))
  (export "save-state" (func $save))
)
"#;

fn host(policy: &str) -> Host {
    Host::builder()
        .manifest(Manifest::parse(policy).expect("policy"))
        .build()
        .expect("host")
}

/// The default policy for these tests: enough fuel to run, nothing granted.
fn plain_host() -> Host {
    host("[limits]\nfuel = 100_000_000\n")
}

fn get(plugin: &mut watoots::Plugin) -> u32 {
    match plugin.call("get", &[]).expect("get").as_slice() {
        [Val::U32(n)] => *n,
        other => panic!("get returned {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// The mechanism
// ---------------------------------------------------------------------------

#[test]
fn a_reload_without_state_hooks_runs_the_new_code() {
    let host = plain_host();
    let mut plugin = host.load_binary("counter", COUNTER_V1.as_bytes()).unwrap();
    plugin.call("bump", &[Val::U32(7)]).unwrap();
    assert_eq!(get(&mut plugin), 7);

    // COUNTER_NO_HOOKS has no `restore-state`, so nothing crosses even though
    // the outgoing build could have handed something over.
    let report = plugin.reload(COUNTER_NO_HOOKS.as_bytes()).unwrap();
    assert!(report.state_saved, "the outgoing build exports save-state");
    assert!(!report.state_restored);
    assert!(report.state_dropped(), "{report:?}");
    assert_eq!(report.reloads, 1);

    // 500 rather than 507: the new code is running and it started from zero.
    assert_eq!(get(&mut plugin), 500);
    assert_eq!(
        plugin.name(),
        "counter",
        "a reload replaces code, not identity"
    );
}

#[test]
fn a_plugin_with_no_hooks_at_all_reloads_carrying_nothing() {
    let host = plain_host();
    let mut plugin = host
        .load_binary("counter", COUNTER_NO_HOOKS.as_bytes())
        .unwrap();
    plugin.call("bump", &[Val::U32(3)]).unwrap();
    assert_eq!(get(&mut plugin), 503);

    let report = plugin.reload(COUNTER_V1.as_bytes()).unwrap();
    assert!(!report.state_saved, "there was no save-state to call");
    assert!(!report.state_restored, "so there was nothing to restore");
    assert_eq!(get(&mut plugin), 0);
}

#[test]
fn state_crosses_when_both_builds_declare_the_hooks() {
    let host = plain_host();
    let mut plugin = host.load_binary("counter", COUNTER_V1.as_bytes()).unwrap();
    plugin.call("bump", &[Val::U32(7)]).unwrap();

    let report = plugin.reload(COUNTER_V2.as_bytes()).unwrap();
    assert!(report.state_saved && report.state_restored, "{report:?}");
    assert!(!report.state_dropped());

    // 1007 is the whole claim in one number: 1000 says the new code is running
    // and 7 says it started from the old instance's state.
    assert_eq!(get(&mut plugin), 1007);
}

#[test]
fn state_crosses_every_reload_not_just_the_first() {
    let host = plain_host();
    let mut plugin = host.load_binary("counter", COUNTER_V1.as_bytes()).unwrap();
    plugin.call("bump", &[Val::U32(1)]).unwrap();

    for expected in 1..=4 {
        let report = plugin.reload(COUNTER_V1.as_bytes()).unwrap();
        assert_eq!(report.reloads, expected);
        assert!(report.state_restored);
    }
    assert_eq!(get(&mut plugin), 1, "the counter survived four reloads");
    assert_eq!(plugin.stats().reloads, 4);
}

#[test]
fn the_hook_names_are_the_ones_the_constants_publish() {
    // A world author reads these off the crate rather than out of a doc
    // comment, so they are part of the API and a rename is a breaking change.
    assert_eq!(SAVE_STATE_EXPORT, "save-state");
    assert_eq!(RESTORE_STATE_EXPORT, "restore-state");
}

// ---------------------------------------------------------------------------
// The old instance survives
// ---------------------------------------------------------------------------

#[test]
fn a_reload_that_wants_an_ungranted_import_is_refused_and_the_old_code_runs_on() {
    // ADR-0010's central claim. New bytes may import more than the old ones
    // did, and a plugin must not acquire a capability by being updated.
    let host = plain_host();
    let mut plugin = host.load_binary("counter", COUNTER_V1.as_bytes()).unwrap();
    plugin.call("bump", &[Val::U32(7)]).unwrap();
    let before = plugin.stats();

    let err = plugin
        .reload(COUNTER_WANTS_NET.as_bytes())
        .expect_err("the manifest grants no sockets");
    assert_eq!(err.kind(), ErrorKind::PermissionDenied);
    assert!(err.message().contains("wasi:sockets"), "{}", err.message());

    // Still the old instance, still holding the old instance's state.
    assert_eq!(get(&mut plugin), 7);
    assert_eq!(plugin.stats().reloads, 0, "nothing was replaced");
    assert_eq!(
        plugin.grants().decisions.len(),
        before.imports_declared,
        "the refused component's imports are not this plugin's"
    );

    // And the replacement is still refused on a second attempt: the failure
    // left nothing half-applied that a retry could slip through.
    assert_eq!(
        plugin
            .reload(COUNTER_WANTS_NET.as_bytes())
            .expect_err("still refused")
            .kind(),
        ErrorKind::PermissionDenied
    );
    assert_eq!(get(&mut plugin), 7);
}

#[test]
fn a_refused_reload_does_not_even_ask_the_old_instance_for_its_state() {
    // The replacement is built first, so a reload refused for asking too much
    // has not called into the running plugin at all. That is what keeps a
    // rejected update from being a way to poke at a plugin's hooks.
    let host = plain_host();
    let mut plugin = host.load_binary("counter", COUNTER_V1.as_bytes()).unwrap();
    let before = plugin.stats().calls;

    plugin
        .reload(COUNTER_WANTS_NET.as_bytes())
        .expect_err("refused");

    assert_eq!(plugin.stats().calls, before, "save-state was never called");
}

#[test]
fn bytes_that_are_not_a_component_leave_the_old_instance_running() {
    let host = plain_host();
    let mut plugin = host.load_binary("counter", COUNTER_V1.as_bytes()).unwrap();
    plugin.call("bump", &[Val::U32(2)]).unwrap();

    let err = plugin
        .reload(b"not wasm at all")
        .expect_err("not a component");
    assert_eq!(err.kind(), ErrorKind::Load);
    assert_eq!(get(&mut plugin), 2);
}

#[test]
fn a_replacement_that_cannot_take_the_state_shape_is_refused() {
    // Not cross-version state migration: the value crosses as one typed shape,
    // and a world that changed it owes its plugins a version bump.
    let host = plain_host();
    let mut plugin = host.load_binary("counter", COUNTER_V1.as_bytes()).unwrap();
    plugin.call("bump", &[Val::U32(9)]).unwrap();

    let err = plugin
        .reload(COUNTER_WRONG_STATE_TYPE.as_bytes())
        .expect_err("u32 does not fit a string parameter");
    assert_eq!(err.kind(), ErrorKind::InvalidArgument);
    assert!(
        err.message().contains(RESTORE_STATE_EXPORT),
        "{}",
        err.message()
    );

    assert_eq!(get(&mut plugin), 9, "the old build is still running");
    assert_eq!(plugin.stats().reloads, 0);
}

#[test]
fn a_hook_the_host_cannot_call_is_refused_rather_than_ignored() {
    // A `save-state` taking an argument is a world that meant something else by
    // the name. Ignoring it would silently lose state on every reload.
    let host = plain_host();
    let mut plugin = host
        .load_binary("counter", COUNTER_BAD_HOOK_SHAPE.as_bytes())
        .unwrap();

    let err = plugin
        .reload(COUNTER_V1.as_bytes())
        .expect_err("the outgoing save-state takes a parameter");
    assert_eq!(err.kind(), ErrorKind::InvalidArgument);
    assert!(
        err.message().contains(SAVE_STATE_EXPORT),
        "{}",
        err.message()
    );
    assert_eq!(get(&mut plugin), 0, "still the old instance");
}

#[test]
fn a_replacement_that_traps_taking_the_state_leaves_the_old_one_working() {
    // The failure furthest into the handoff that still costs nothing: the
    // outgoing instance has already answered, the replacement has been built,
    // and it traps on the way in. The replacement is dropped and the plugin is
    // exactly where it was.
    let host = plain_host();
    let mut plugin = host.load_binary("counter", COUNTER_V1.as_bytes()).unwrap();
    plugin.call("bump", &[Val::U32(8)]).unwrap();

    let err = plugin
        .reload(COUNTER_RESTORE_TRAPS.as_bytes())
        .expect_err("restore-state traps");
    assert_eq!(err.kind(), ErrorKind::Trap);

    assert_eq!(get(&mut plugin), 8, "the old instance is still callable");
    assert_eq!(plugin.stats().reloads, 0);

    // And it can still be reloaded afterwards: a failed attempt is not a state
    // the plugin has to be rescued from.
    plugin.reload(COUNTER_V2.as_bytes()).unwrap();
    assert_eq!(get(&mut plugin), 1008);
}

#[test]
fn state_is_bounded_by_limits_transfer_like_any_other_crossing() {
    // The third of ADR-0010's arguments for a typed value: the host can see it,
    // so it can refuse it. Nothing here implements a cap — `save-state` is an
    // ordinary crossing and the manifest's ceiling already applies to it.
    let host = host("[limits]\nfuel = 100_000_000\ntransfer = 1024\n");
    let mut plugin = host
        .load_binary("counter", COUNTER_HUGE_STATE.as_bytes())
        .unwrap();

    let err = plugin
        .reload(COUNTER_HUGE_STATE.as_bytes())
        .expect_err("60kB of state against a 1kB ceiling");
    // Both the message and the kind. Wasmtime reports an exhausted hostcall
    // budget as a trap carrying a private type, so watoots recognises it by
    // matching the message — which makes this assertion the thing that notices
    // if wasmtime ever rewords it. `asset_e2e` pins the same pair, but only
    // when a guest has been built; this one is WAT and always runs, so it is
    // the guard that holds in CI.
    assert!(err.message().contains("hostcall"), "{}", err.message());
    assert_eq!(
        err.kind(),
        ErrorKind::LimitExceeded,
        "a spent ceiling is a limit, not misbehaviour: {}",
        err.message()
    );

    // The plugin was not replaced — but this is the one failure that costs the
    // running instance anyway, and the test says so rather than claiming more
    // than reload can deliver. `save-state` *trapped*, and Wasmtime 48 refuses
    // to re-enter a component instance after any trap; that is true of a
    // trapping `lint` call too, and reload neither causes it nor can undo it.
    assert_eq!(plugin.stats().reloads, 0, "nothing was replaced");
    assert_eq!(plugin.name(), "counter");
    let after = plugin.call("get", &[]).expect_err("the trap poisoned it");
    assert!(
        after.message().contains("cannot enter component instance"),
        "{}",
        after.message()
    );
}

// ---------------------------------------------------------------------------
// Counters
// ---------------------------------------------------------------------------

#[test]
fn stats_accumulate_across_a_reload() {
    // These answer "what has this plugin cost me". A reload is a new build of
    // the same plugin, so resetting would let a host that reloads on every file
    // change report near-zero forever.
    let host = plain_host();
    let mut plugin = host.load_binary("counter", COUNTER_V1.as_bytes()).unwrap();
    plugin.call("bump", &[Val::U32(1)]).unwrap();
    plugin.call("bump", &[Val::U32(1)]).unwrap();

    let before = plugin.stats();
    assert_eq!(before.calls, 2);
    assert_eq!(before.reloads, 0);
    assert!(before.fuel_consumed > 0);

    plugin.reload(COUNTER_V1.as_bytes()).unwrap();
    let after = plugin.stats();

    // Two calls, plus the two the handoff itself made: the hooks are ordinary
    // crossings that burn fuel, and hiding them would make the fuel total not
    // add up.
    assert_eq!(after.calls, 4, "{after:?}");
    assert_eq!(after.reloads, 1);
    assert!(
        after.fuel_consumed >= before.fuel_consumed,
        "fuel must not go backwards: {before:?} then {after:?}"
    );
}

#[test]
fn the_peak_memory_high_water_mark_does_not_fall_at_a_reload() {
    let host = plain_host();
    let mut plugin = host
        .load_binary("counter", COUNTER_HUGE_STATE.as_bytes())
        .unwrap();
    let peak = plugin.stats().peak_memory_bytes;
    assert!(peak > 0, "the component has a memory");

    // COUNTER_NO_HOOKS has no memory at all, so a peak that tracked only the
    // running instance would drop to zero. A high-water mark does not.
    plugin.reload(COUNTER_NO_HOOKS.as_bytes()).unwrap();
    assert_eq!(plugin.stats().peak_memory_bytes, peak);
}

#[test]
fn imports_declared_describes_the_bytes_running_now() {
    let host = plain_host();
    let mut plugin = host
        .load_binary("counter", COUNTER_NO_HOOKS.as_bytes())
        .unwrap();
    assert_eq!(plugin.stats().imports_declared, 0);

    plugin.reload(COUNTER_V1.as_bytes()).unwrap();
    let stats = plugin.stats();
    assert_eq!(stats.imports_declared, plugin.grants().decisions.len());
    assert_eq!(stats.imports_denied, 0);
}

#[test]
fn the_profile_starts_again_at_a_reload() {
    // The opposite decision to `stats`, for the opposite reason: a profile
    // attributes time to code, and one row averaging two builds of an export
    // describes neither.
    let host = Host::builder()
        .manifest(Manifest::parse("[limits]\nfuel = 100_000_000\n").unwrap())
        .profile()
        .build()
        .unwrap();
    let mut plugin = host.load_binary("counter", COUNTER_V1.as_bytes()).unwrap();

    get(&mut plugin);
    get(&mut plugin);
    assert_eq!(plugin.profile().unwrap().calls, 2);

    plugin.reload(COUNTER_V1.as_bytes()).unwrap();

    // One call: `restore-state`, made after the replacement existed. The two
    // `get`s and the outgoing build's `save-state` are gone with the instance
    // that ran them.
    let profile = plugin.profile().unwrap();
    assert_eq!(profile.calls, 1, "{profile:?}");
    assert_eq!(
        profile.functions.len(),
        1,
        "only the replacement's own rows: {:?}",
        profile.functions
    );
    assert_eq!(profile.functions[0].func, RESTORE_STATE_EXPORT);

    // And the two answers diverge, which is the visible consequence of their
    // answering different questions.
    let stats: PluginStats = plugin.stats();
    assert!(stats.calls > profile.calls, "{stats:?} vs {profile:?}");
}

// ---------------------------------------------------------------------------
// From a file, and through a registry
// ---------------------------------------------------------------------------

#[test]
fn reload_from_file_replaces_the_code_and_keeps_the_name() {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("counter.wat");
    let second = dir.path().join("counter-v2.wat");
    std::fs::write(&first, COUNTER_V1).unwrap();
    std::fs::write(&second, COUNTER_V2).unwrap();

    let host = plain_host();
    let mut plugin = host.load(&first).unwrap();
    plugin.call("bump", &[Val::U32(5)]).unwrap();

    plugin.reload_from_file(&second).unwrap();
    assert_eq!(plugin.name(), "counter", "the file was named differently");
    assert_eq!(get(&mut plugin), 1005);
}

#[test]
fn a_missing_file_leaves_the_old_instance_running() {
    let host = plain_host();
    let mut plugin = host.load_binary("counter", COUNTER_V1.as_bytes()).unwrap();
    plugin.call("bump", &[Val::U32(4)]).unwrap();

    let err = plugin
        .reload_from_file("no/such/component.wasm")
        .expect_err("there is no such file");
    assert_eq!(err.kind(), ErrorKind::NotFound);
    assert_eq!(get(&mut plugin), 4);
}

#[test]
fn a_registry_reload_never_leaves_a_gap() {
    let mut registry = Registry::new(plain_host());
    registry
        .load_binary("counter", COUNTER_V1.as_bytes())
        .unwrap();
    registry.call("counter", "bump", &[Val::U32(6)]).unwrap();

    // A refused reload keeps the plugin registered and callable, which is the
    // registry half of "better a plugin that cannot be replaced than none".
    registry
        .reload_binary("counter", COUNTER_WANTS_NET.as_bytes())
        .expect_err("refused");
    assert_eq!(registry.len(), 1);
    assert_eq!(registry.names().collect::<Vec<_>>(), ["counter"]);
    assert_eq!(registry.call("counter", "get", &[]).unwrap(), [Val::U32(6)]);

    let report = registry
        .reload_binary("counter", COUNTER_V2.as_bytes())
        .unwrap();
    assert!(report.state_restored);
    assert_eq!(registry.len(), 1);
    assert_eq!(
        registry.call("counter", "get", &[]).unwrap(),
        [Val::U32(1006)]
    );
}

#[test]
fn reloading_a_plugin_that_is_not_registered_says_so() {
    let mut registry = Registry::new(plain_host());
    let err = registry
        .reload_binary("absent", COUNTER_V1.as_bytes())
        .expect_err("nothing is registered");
    assert_eq!(err.kind(), ErrorKind::NotFound);
}
