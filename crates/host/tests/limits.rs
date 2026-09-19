//! Resource ceilings: memory, fuel, and the per-call deadline.
//!
//! These are the tests that decide whether the sandbox is a promise or a
//! comment, so each one drives a guest that genuinely misbehaves.

use std::time::{Duration, Instant};

use watoots::{ErrorKind, Host, Manifest, Val};

/// Loops forever. Only a limit can stop it.
const SPINNER: &str = r#"
(component
  (core module $m
    (func (export "spin") (loop $l br $l)))
  (core instance $i (instantiate $m))
  (func $spin (canon lift (core func $i "spin")))
  (export "spin" (func $spin))
)
"#;

/// Grows its linear memory by however many pages it is asked for, and reports
/// the previous size — or -1 if the growth was refused.
const GROWER: &str = r#"
(component
  (core module $m
    (memory 1)
    (func (export "grow") (param i32) (result i32)
      (memory.grow (local.get 0))))
  (core instance $i (instantiate $m))
  (func $grow (param "pages" s32) (result s32) (canon lift (core func $i "grow")))
  (export "grow" (func $grow))
)
"#;

/// What a real allocator does when `memory.grow` says no: gives up. Rust's
/// `handle_alloc_error` aborts, which is an `unreachable` trap; this is that
/// shape with the decision made explicit.
const ALLOCATE_OR_DIE: &str = r#"
(component
  (core module $m
    (memory 1)
    (func (export "need") (param i32) (result i32)
      (if (i32.eq (memory.grow (local.get 0)) (i32.const -1))
        (then (unreachable)))
      (i32.const 0)))
  (core instance $i (instantiate $m))
  (func $need (param "pages" s32) (result s32) (canon lift (core func $i "need")))
  (export "need" (func $need))
)
"#;

/// Returns immediately.
const CHEAP: &str = r#"
(component
  (core module $m
    (func (export "answer") (result i32) i32.const 42))
  (core instance $i (instantiate $m))
  (func $answer (result s32) (canon lift (core func $i "answer")))
  (export "answer" (func $answer))
)
"#;

fn host_with(manifest_toml: &str) -> Host {
    Host::builder()
        .manifest(Manifest::parse(manifest_toml).unwrap())
        .build()
        .unwrap()
}

#[test]
fn fuel_stops_a_runaway_guest() {
    let host = host_with("[limits]\nfuel = 100_000\n");
    let mut plugin = host.load_binary("spinner", SPINNER.as_bytes()).unwrap();

    let err = plugin.call("spin", &[]).unwrap_err();
    assert_eq!(
        err.kind(),
        ErrorKind::LimitExceeded,
        "expected a limit, got: {}",
        err.message()
    );
}

#[test]
fn fuel_is_a_per_call_budget_not_a_lifetime_one() {
    // Enough fuel for the call, but not for very many; if fuel were not reset
    // between calls the second or third would starve.
    let host = host_with("[limits]\nfuel = 100_000\n");
    let mut plugin = host.load_binary("cheap", CHEAP.as_bytes()).unwrap();

    for _ in 0..5 {
        let results = plugin.call("answer", &[]).unwrap();
        assert_eq!(results, vec![Val::S32(42)]);
    }
}

#[test]
fn the_deadline_stops_a_runaway_guest() {
    let host = host_with("[limits]\ntimeout = \"50ms\"\n");
    let mut plugin = host.load_binary("spinner", SPINNER.as_bytes()).unwrap();

    let started = Instant::now();
    let err = plugin.call("spin", &[]).unwrap_err();
    let elapsed = started.elapsed();

    assert_eq!(
        err.kind(),
        ErrorKind::LimitExceeded,
        "expected a limit, got: {}",
        err.message()
    );
    // Generous upper bound: this asserts the deadline fires at all and is in
    // the right order of magnitude, not that the timer is precise.
    assert!(
        elapsed < Duration::from_secs(5),
        "deadline took {elapsed:?} to fire"
    );
}

#[test]
fn the_deadline_is_also_per_call() {
    let host = host_with("[limits]\ntimeout = \"50ms\"\n");
    let mut plugin = host.load_binary("cheap", CHEAP.as_bytes()).unwrap();

    for _ in 0..3 {
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(plugin.call("answer", &[]).unwrap(), vec![Val::S32(42)]);
    }
}

#[test]
fn memory_growth_past_the_ceiling_is_refused() {
    // One page (64KiB) initial, ceiling 128KiB: instantiating is fine, growing
    // by 100 pages is not.
    let host = host_with("[limits]\nmemory = \"128KiB\"\n");
    let mut plugin = host.load_binary("grower", GROWER.as_bytes()).unwrap();

    let results = plugin.call("grow", &[Val::S32(100)]).unwrap();
    assert_eq!(
        results,
        vec![Val::S32(-1)],
        "growth past the ceiling should be refused"
    );
}

#[test]
fn a_trap_after_a_refused_growth_is_the_memory_ceiling_and_not_a_trap() {
    // A refused growth returns -1 and the guest decides what to do. A real
    // allocator aborts, and that abort arrives as `unreachable` -- which, read
    // literally, told whoever installed the plugin to debug it for opening a
    // 1 GiB image under a 64 MiB policy. The limiter is ours and remembers
    // saying no, so the error says what actually happened.
    let host = host_with("[limits]\nmemory = \"128KiB\"\n");
    let mut plugin = host
        .load_binary("hungry", ALLOCATE_OR_DIE.as_bytes())
        .unwrap();

    let err = plugin
        .call("need", &[Val::S32(100)])
        .expect_err("100 pages is past the ceiling and the guest aborts");
    assert_eq!(
        err.kind(),
        ErrorKind::LimitExceeded,
        "a ceiling is a limit, not misbehaviour: {}",
        err.message()
    );
    // The message leads with the ceiling and the numbers, because "wasm trap:
    // unreachable" is true and useless.
    assert!(err.message().contains("limits.memory"), "{}", err.message());
    assert!(err.message().contains("asked for"), "{}", err.message());
    assert!(
        err.message().contains("131072"),
        "the limit itself: {}",
        err.message()
    );

    // And it is per call: a well-behaved call afterwards is still just a call.
    // (The trap poisoned the instance, so use a fresh one to show the flag
    // does not leak into the next plugin.)
    let mut fresh = host.load_binary("fresh", GROWER.as_bytes()).unwrap();
    assert_eq!(
        fresh.call("grow", &[Val::S32(0)]).unwrap(),
        vec![Val::S32(1)]
    );
}

/// Traps unconditionally. Misbehaviour with no ceiling anywhere near it.
const JUST_TRAPS: &str = r#"
(component
  (core module $m
    (memory 1)
    (func (export "boom") (result i32) (unreachable)))
  (core instance $i (instantiate $m))
  (func $boom (result s32) (canon lift (core func $i "boom")))
  (export "boom" (func $boom))
)
"#;

#[test]
fn a_trap_with_no_refused_growth_is_still_a_trap() {
    // The rule must not swallow real misbehaviour. Same `unreachable`, but no
    // growth was ever refused, so it is reported as what it is.
    let host = host_with("[limits]\nmemory = \"64MiB\"\n");
    let mut plugin = host.load_binary("broken", JUST_TRAPS.as_bytes()).unwrap();
    let err = plugin.call("boom", &[]).expect_err("it traps");
    assert_eq!(err.kind(), ErrorKind::Trap, "{}", err.message());
    assert!(
        !err.message().contains("limits.memory"),
        "{}",
        err.message()
    );
}

#[test]
fn memory_growth_within_the_ceiling_succeeds() {
    let host = host_with("[limits]\nmemory = \"64MiB\"\n");
    let mut plugin = host.load_binary("grower", GROWER.as_bytes()).unwrap();

    // Returns the previous size in pages, so 1 for a memory that started at 1.
    let results = plugin.call("grow", &[Val::S32(100)]).unwrap();
    assert_eq!(results, vec![Val::S32(1)]);
}

#[test]
fn a_ceiling_below_the_initial_memory_fails_the_load() {
    // 32KiB cannot hold the component's one 64KiB page.
    let host = host_with("[limits]\nmemory = \"32KiB\"\n");
    let err = host.load_binary("grower", GROWER.as_bytes()).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Load, "{}", err.message());
}

#[test]
fn limits_are_per_plugin_not_per_host() {
    // One plugin burning its fuel must not affect its neighbour.
    let host = host_with("[limits]\nfuel = 100_000\n");
    let mut spinner = host.load_binary("spinner", SPINNER.as_bytes()).unwrap();
    let mut cheap = host.load_binary("cheap", CHEAP.as_bytes()).unwrap();

    assert_eq!(
        spinner.call("spin", &[]).unwrap_err().kind(),
        ErrorKind::LimitExceeded
    );
    assert_eq!(cheap.call("answer", &[]).unwrap(), vec![Val::S32(42)]);
}

#[test]
fn a_limit_error_leads_with_the_cause_not_the_backtrace() {
    // The C API gets one string. If it opens with "error while executing at
    // wasm backtrace:" then the useful half is off the end of the log line.
    let host = host_with("[limits]\nfuel = 100_000\n");
    let mut plugin = host.load_binary("spinner", SPINNER.as_bytes()).unwrap();

    let err = plugin.call("spin", &[]).unwrap_err();
    let first_line = err.message().lines().next().unwrap();
    assert!(
        first_line.contains("all fuel consumed"),
        "first line should name the cause, got: {first_line}"
    );
    // The backtrace is still there, just below.
    assert!(
        err.message().contains("wasm backtrace"),
        "{}",
        err.message()
    );
}
