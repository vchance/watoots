//! `Plugin::stats`: the half of "metrics" ADR-0006 says to build — observed at
//! the boundary, never reported by the guest.

use watoots::{Host, Manifest, Val};

/// Burns fuel in a loop bounded by its argument, then returns.
const BUSY: &str = r#"
(component
  (core module $m
    (func (export "spin") (param i32)
      (local $i i32)
      (block $done
        (loop $again
          (br_if $done (i32.ge_s (local.get $i) (local.get 0)))
          (local.set $i (i32.add (local.get $i) (i32.const 1)))
          (br $again)))))
  (core instance $i (instantiate $m))
  (func $spin (param "n" s32) (canon lift (core func $i "spin")))
  (export "spin" (func $spin))
)
"#;

fn host(policy: &str) -> Host {
    Host::builder()
        .manifest(Manifest::parse(policy).expect("policy"))
        .build()
        .expect("host")
}

#[test]
fn a_fresh_plugin_has_counted_nothing() {
    let host = host("[limits]\nfuel = 100_000_000\n");
    let plugin = host.load_binary("busy", BUSY.as_bytes()).unwrap();
    let stats = plugin.stats();
    assert_eq!(stats.calls, 0);
    assert_eq!(stats.fuel_consumed, 0);
    assert_eq!(stats.log_messages, 0);
}

#[test]
fn fuel_consumed_grows_with_the_work_done() {
    let host = host("[limits]\nfuel = 100_000_000\n");
    let mut plugin = host.load_binary("busy", BUSY.as_bytes()).unwrap();

    plugin.call("spin", &[Val::S32(10)]).unwrap();
    let small = plugin.stats();
    assert_eq!(small.calls, 1);
    assert!(small.fuel_consumed > 0, "{small:?}");

    plugin.call("spin", &[Val::S32(10_000)]).unwrap();
    let large = plugin.stats();
    assert_eq!(large.calls, 2);
    assert!(
        large.fuel_consumed > small.fuel_consumed * 2,
        "more work must cost more fuel: {small:?} then {large:?}"
    );
}

#[test]
fn a_call_that_ran_out_of_fuel_is_still_counted() {
    // The calls an operator most wants accounted for are the ones that failed.
    let host = host("[limits]\nfuel = 100_000\n");
    let mut plugin = host.load_binary("busy", BUSY.as_bytes()).unwrap();

    plugin
        .call("spin", &[Val::S32(i32::MAX)])
        .expect_err("the budget is far too small for that many iterations");

    let stats = plugin.stats();
    assert_eq!(stats.calls, 1, "a trapped call is still a call");
    assert!(stats.fuel_consumed > 0, "{stats:?}");
}

#[test]
fn an_unmetered_store_reports_no_fuel_rather_than_failing() {
    // No `fuel` key: nothing is metered, so the honest answer is zero.
    let host = host("[limits]\nmemory = \"16MiB\"\n");
    let mut plugin = host.load_binary("busy", BUSY.as_bytes()).unwrap();
    plugin.call("spin", &[Val::S32(1)]).unwrap();
    let stats = plugin.stats();
    assert_eq!(stats.calls, 1);
    assert_eq!(stats.fuel_consumed, 0);
}

#[test]
fn imports_are_reported_from_the_grant_report() {
    let host = host("[limits]\nfuel = 100_000_000\n");
    let plugin = host.load_binary("busy", BUSY.as_bytes()).unwrap();
    let stats = plugin.stats();
    assert_eq!(stats.imports_declared, plugin.grants().decisions.len());
    assert_eq!(stats.imports_denied, 0, "it loaded, so nothing was denied");
}

/// Starts at one page and grows by the number of pages asked for.
const GROWS: &str = r#"
(component
  (core module $m
    (memory (export "mem") 1)
    (func (export "grow") (param i32)
      (drop (memory.grow (local.get 0)))))
  (core instance $i (instantiate $m))
  (func $grow (param "pages" s32) (canon lift (core func $i "grow")))
  (export "grow" (func $grow))
)
"#;

#[test]
fn peak_memory_records_the_high_water_mark_not_the_ceiling() {
    let host = host("[limits]\nmemory = \"16MiB\"\nfuel = 100_000_000\n");
    let mut plugin = host.load_binary("grows", GROWS.as_bytes()).unwrap();

    // One page exists before anything is called: the limiter sees the
    // instantiation, not just later growth.
    let start = plugin.stats().peak_memory_bytes;
    assert_eq!(start, 64 * 1024, "one page at instantiation");

    plugin.call("grow", &[Val::S32(4)]).unwrap();
    let grown = plugin.stats().peak_memory_bytes;
    assert_eq!(grown, 5 * 64 * 1024, "one page plus four");

    // It is a high-water mark, so it does not fall back. There is no shrink in
    // wasm, but the point is that it reports what was reached rather than what
    // is held now, and never the 16MiB ceiling.
    plugin.call("grow", &[Val::S32(0)]).unwrap();
    assert_eq!(plugin.stats().peak_memory_bytes, grown);
    assert!(grown < 16 * 1024 * 1024, "the ceiling is not the answer");
}

#[test]
fn a_refused_growth_does_not_raise_the_peak() {
    // 1MiB ceiling, then ask for 100 pages more than fits. The guest sees
    // memory.grow return -1; the peak must reflect what was granted, not what
    // was requested, or a plugin could inflate the number by asking.
    let host = host("[limits]\nmemory = \"1MiB\"\nfuel = 100_000_000\n");
    let mut plugin = host.load_binary("grows", GROWS.as_bytes()).unwrap();

    plugin.call("grow", &[Val::S32(4)]).unwrap();
    let honest = plugin.stats().peak_memory_bytes;

    plugin.call("grow", &[Val::S32(1_000)]).unwrap();
    assert_eq!(
        plugin.stats().peak_memory_bytes,
        honest,
        "a growth the limiter refused must not count"
    );
}
