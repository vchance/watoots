//! `Plugin::profile`: where a plugin's time went, split at the boundary.
//!
//! The first test in this file is the one ADR-0009 says has to exist. Sampling
//! is driven from the same epoch deadline that stops a runaway plugin, and if
//! the two ever stop sharing it correctly the sandbox has a hole in it that no
//! other test would notice.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use watoots::{ErrorKind, FunctionKind, Host, Manifest, Val};

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

/// Burns a bounded amount of time inside wasm and returns.
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

/// Calls `wasi:logging`'s `log`, `count` times. Lifted verbatim from
/// `tests/logging.rs`, where the comments explaining the index-based instance
/// type live.
const LOGS: &str = r#"
(component
  ;; Index-based rather than named, because an instance type used as an import
  ;; may only reference types it also exports, and the text format has no way to
  ;; bind a name to the *exported* enum -- `(export "level" (type (eq $l)))`
  ;; introduces a second index that only a number can reach. This is the shape
  ;; `wasm-tools print` emits for a real wit-bindgen guest, comments and all.
  (type (;0;)
    (instance
      (type (;0;) (enum "trace" "debug" "info" "warn" "error" "critical"))
      (export (;1;) "level" (type (eq 0)))
      (type (;2;) (func (param "level" 1) (param "context" string) (param "message" string)))
      (export (;0;) "log" (func (type 2)))
    )
  )
  (import "wasi:logging/logging@0.1.0-draft" (instance $log (type 0)))
  (alias export $log "log" (func $log_fn))
  (alias export $log "level" (type $level_t))

  (core module $libc
    (memory (export "memory") 1)
    (func (export "realloc") (param i32 i32 i32 i32) (result i32)
      i32.const 256)
  )
  (core instance $libc_i (instantiate $libc))

  (core func $log_lowered
    (canon lower (func $log_fn)
      (memory $libc_i "memory")
      (realloc (func $libc_i "realloc"))))

  (core module $m
    (import "log" "log" (func $log (param i32 i32 i32 i32 i32)))
    (import "libc" "memory" (memory 1))
    (data (i32.const 0) "ctx")
    (data (i32.const 8) "hello")
    (func (export "emit") (param $level i32) (param $count i32)
      (local $i i32)
      (block $done
        (loop $again
          (br_if $done (i32.ge_u (local.get $i) (local.get $count)))
          (call $log
            (local.get $level)
            (i32.const 0) (i32.const 3)
            (i32.const 8) (i32.const 5))
          (local.set $i (i32.add (local.get $i) (i32.const 1)))
          (br $again))))
  )
  (core instance $log_i (export "log" (func $log_lowered)))
  (core instance $i (instantiate $m
    (with "log" (instance $log_i))
    (with "libc" (instance $libc_i))))

  (func $emit (param "level" $level_t) (param "count" u32)
    (canon lift (core func $i "emit")))
  (export "emit" (func $emit))
)
"#;

/// Calls a host function `n` times.
const CALLS_OUT: &str = r#"
(component
  (import "app:demo/sleep" (instance $host
    (export "tick" (func))))
  (core module $m
    (import "host" "tick" (func $tick))
    (func (export "run") (param i32)
      (local $i i32)
      (block $done
        (loop $again
          (br_if $done (i32.ge_s (local.get $i) (local.get 0)))
          (call $tick)
          (local.set $i (i32.add (local.get $i) (i32.const 1)))
          (br $again)))))
  (core func $tick (canon lower (func $host "tick")))
  (core instance $hi (instantiate $m
    (with "host" (instance (export "tick" (func $tick))))))
  (func $run (param "n" s32) (canon lift (core func $hi "run")))
  (export "run" (func $run))
)
"#;

// ---------------------------------------------------------------------------
// The deadline stays authoritative
// ---------------------------------------------------------------------------

/// The test ADR-0009 asks for: a profiled runaway plugin still dies on time.
///
/// Run on its own thread with a hard receive timeout, because the failure this
/// guards against is a plugin that never stops — and a test that hangs reports
/// nothing. The worker is abandoned rather than joined; the harness exits the
/// process when the suite finishes.
#[test]
fn sampling_never_lets_a_plugin_outlive_its_timeout() {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let host = Host::builder()
            .manifest(Manifest::parse("[limits]\ntimeout = \"100ms\"\n").unwrap())
            // Sampled twenty times inside that budget: every sample is an
            // opportunity to reset the deadline and lose the timeout.
            .profile_guest_samples(Duration::from_millis(5))
            .build()
            .unwrap();
        let mut plugin = host.load_binary("spinner", SPINNER.as_bytes()).unwrap();

        let started = Instant::now();
        let outcome = plugin.call("spin", &[]);
        let _ = tx.send((started.elapsed(), outcome.map(|_| ()).map_err(|e| e.kind())));
    });

    let (elapsed, outcome) = rx
        .recv_timeout(Duration::from_secs(30))
        .expect("the deadline never fired: sampling reset it forever");

    assert_eq!(
        outcome,
        Err(ErrorKind::LimitExceeded),
        "a spinning guest must hit its timeout, profiled or not"
    );
    // Generous, like the unprofiled deadline test next door: this asserts the
    // budget is still spent once rather than once per sample. Twenty samples
    // each granting a fresh 100ms would land at two seconds.
    assert!(
        elapsed < Duration::from_secs(2),
        "the deadline took {elapsed:?} to fire under sampling; \
         the timeout budget is being re-granted"
    );
}

#[test]
fn a_sample_interval_longer_than_the_timeout_does_not_win() {
    // The interval would carry the plugin well past its budget if the callback
    // used it rather than the smaller of the two.
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let host = Host::builder()
            .manifest(Manifest::parse("[limits]\ntimeout = \"100ms\"\n").unwrap())
            .profile_guest_samples(Duration::from_secs(60))
            .build()
            .unwrap();
        let mut plugin = host.load_binary("spinner", SPINNER.as_bytes()).unwrap();
        let started = Instant::now();
        let kind = plugin.call("spin", &[]).map(|_| ()).map_err(|e| e.kind());
        let _ = tx.send((started.elapsed(), kind));
    });

    let (elapsed, outcome) = rx
        .recv_timeout(Duration::from_secs(30))
        .expect("the deadline never fired: the sample interval displaced it");
    assert_eq!(outcome, Err(ErrorKind::LimitExceeded));
    assert!(elapsed < Duration::from_secs(2), "{elapsed:?}");
}

#[test]
fn the_timeout_is_still_per_call_when_sampling() {
    // The budget is re-armed per call. If the callback drew it down without
    // `arm` restoring it, the third call here would trap.
    let host = Host::builder()
        .manifest(Manifest::parse("[limits]\ntimeout = \"200ms\"\n").unwrap())
        .profile_guest_samples(Duration::from_millis(1))
        .build()
        .unwrap();
    let mut plugin = host.load_binary("busy", BUSY.as_bytes()).unwrap();

    for _ in 0..5 {
        plugin
            .call("spin", &[Val::S32(50_000)])
            .expect("well inside the budget");
    }
}

#[test]
fn profiling_without_sampling_leaves_the_deadline_alone() {
    // No epoch callback is installed at all in this configuration, so this is
    // the unmodified path — worth pinning, because it is the one every
    // profiled-but-not-sampled plugin takes.
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let host = Host::builder()
            .manifest(Manifest::parse("[limits]\ntimeout = \"100ms\"\n").unwrap())
            .profile()
            .build()
            .unwrap();
        let mut plugin = host.load_binary("spinner", SPINNER.as_bytes()).unwrap();
        let _ = tx.send(plugin.call("spin", &[]).map(|_| ()).map_err(|e| e.kind()));
    });

    assert_eq!(
        rx.recv_timeout(Duration::from_secs(30))
            .expect("the deadline never fired"),
        Err(ErrorKind::LimitExceeded)
    );
}

#[test]
fn fuel_still_stops_a_profiled_guest() {
    // Fuel and the deadline are independent budgets; profiling touches only
    // one of them, and this says so.
    let host = Host::builder()
        .manifest(Manifest::parse("[limits]\nfuel = 100_000\n").unwrap())
        .profile_guest_samples(Duration::from_millis(1))
        .build()
        .unwrap();
    let mut plugin = host.load_binary("spinner", SPINNER.as_bytes()).unwrap();
    assert_eq!(
        plugin.call("spin", &[]).unwrap_err().kind(),
        ErrorKind::LimitExceeded
    );
}

// ---------------------------------------------------------------------------
// The three buckets
// ---------------------------------------------------------------------------

fn profiled(policy: &str) -> Host {
    Host::builder()
        .manifest(Manifest::parse(policy).unwrap())
        .profile()
        .build()
        .unwrap()
}

#[test]
fn profiling_is_refused_without_being_asked_for() {
    let host = Host::builder().build().unwrap();
    let plugin = host.load_binary("busy", BUSY.as_bytes()).unwrap();
    let err = plugin
        .profile()
        .expect_err("a page of zeroes is a worse answer than being told it is off");
    assert_eq!(err.kind(), ErrorKind::InvalidArgument);
    assert!(err.message().contains("profiling is not enabled"));
}

#[test]
fn a_fresh_plugin_has_profiled_nothing() {
    let host = profiled("");
    let plugin = host.load_binary("busy", BUSY.as_bytes()).unwrap();
    let profile = plugin.profile().unwrap();
    assert_eq!(profile.calls, 0);
    assert_eq!(profile.wall_nanos, 0);
    assert!(profile.functions.is_empty());
}

#[test]
fn a_compute_bound_guest_spends_its_time_in_the_guest_bucket() {
    let host = profiled("");
    let mut plugin = host.load_binary("busy", BUSY.as_bytes()).unwrap();

    plugin.call("spin", &[Val::S32(20_000_000)]).unwrap();
    let profile = plugin.profile().unwrap();

    assert_eq!(profile.calls, 1);
    assert!(profile.wall_nanos > 0, "{profile:?}");
    assert_eq!(profile.host_nanos, 0, "it calls nothing out: {profile:?}");
    assert!(
        profile.guest_nanos * 2 > profile.wall_nanos,
        "a guest that only computes should dominate its own wall time: {profile:?}"
    );
}

#[test]
fn the_buckets_add_up_to_the_wall_time() {
    // Not an accounting identity anybody measured: marshalling is defined as
    // the remainder, and this pins that definition rather than a measurement.
    let host = profiled("");
    let mut plugin = host.load_binary("busy", BUSY.as_bytes()).unwrap();
    plugin.call("spin", &[Val::S32(100_000)]).unwrap();

    let profile = plugin.profile().unwrap();
    assert_eq!(
        profile.guest_nanos + profile.host_nanos + profile.marshalling_nanos,
        profile.wall_nanos
    );
}

#[test]
fn more_work_costs_more_guest_time() {
    let host = profiled("");
    let mut plugin = host.load_binary("busy", BUSY.as_bytes()).unwrap();

    plugin.call("spin", &[Val::S32(1_000)]).unwrap();
    let small = plugin.profile().unwrap().guest_nanos;
    plugin.call("spin", &[Val::S32(20_000_000)]).unwrap();
    let large = plugin.profile().unwrap().guest_nanos;

    assert!(
        large > small * 2,
        "twenty thousand times the work should cost more guest time: {small} then {large}"
    );
}

#[test]
fn a_call_that_trapped_is_still_profiled() {
    // Same argument as PluginStats: the calls an operator most wants accounted
    // for are the ones that failed.
    let host = Host::builder()
        .manifest(Manifest::parse("[limits]\nfuel = 100_000\n").unwrap())
        .profile()
        .build()
        .unwrap();
    let mut plugin = host.load_binary("spinner", SPINNER.as_bytes()).unwrap();

    plugin.call("spin", &[]).expect_err("out of fuel");
    let profile = plugin.profile().unwrap();
    assert_eq!(profile.calls, 1, "a trapped call is still a call");
    assert!(profile.wall_nanos > 0, "{profile:?}");
    assert_eq!(profile.functions.len(), 1);
}

// ---------------------------------------------------------------------------
// Per-WIT-function attribution
// ---------------------------------------------------------------------------

#[test]
fn each_export_gets_its_own_row() {
    let host = profiled("");
    let mut plugin = host.load_binary("busy", BUSY.as_bytes()).unwrap();

    plugin.call("spin", &[Val::S32(1_000)]).unwrap();
    plugin.call("spin", &[Val::S32(1_000)]).unwrap();

    let profile = plugin.profile().unwrap();
    assert_eq!(profile.functions.len(), 1);
    let row = &profile.functions[0];
    assert_eq!(row.kind, FunctionKind::Export);
    assert_eq!(row.func, "spin");
    assert_eq!(row.interface, "", "an export is named without one");
    assert_eq!(row.calls, 2);
    assert_eq!(row.wall_nanos, profile.wall_nanos);
}

#[test]
fn a_host_call_is_attributed_to_the_import_that_served_it() {
    let host = Host::builder()
        .host_func("app:demo/sleep", "tick", |_call| {
            std::thread::sleep(Duration::from_millis(2));
            Ok(Vec::new())
        })
        .profile()
        .build()
        .unwrap();
    let mut plugin = host.load_binary("caller", CALLS_OUT.as_bytes()).unwrap();

    plugin.call("run", &[Val::S32(5)]).unwrap();
    let profile = plugin.profile().unwrap();

    // Ten milliseconds of deliberate sleep inside the host function: it has to
    // land in the host bucket and nowhere else.
    assert!(
        profile.host_nanos > 5_000_000,
        "the host function slept 10ms: {profile:?}"
    );
    assert!(
        profile.guest_nanos < profile.host_nanos,
        "the guest does nothing but call out: {profile:?}"
    );

    let import = profile
        .functions
        .iter()
        .find(|row| row.kind == FunctionKind::Import)
        .unwrap_or_else(|| panic!("no import row in {profile:?}"));
    assert_eq!(import.interface, "app:demo/sleep");
    assert_eq!(import.func, "tick");
    assert_eq!(import.calls, 5);
    assert!(import.host_nanos > 5_000_000, "{import:?}");
    assert_eq!(import.guest_nanos, 0, "a host function has no guest time");

    let export = profile
        .functions
        .iter()
        .find(|row| row.kind == FunctionKind::Export)
        .unwrap();
    assert!(
        import.host_nanos <= export.host_nanos,
        "the import's time is part of the export's: {profile:?}"
    );
}

#[test]
fn logging_is_attributed_like_any_other_import() {
    let host = Host::builder()
        .manifest(Manifest::parse("[permissions]\nlogging = \"trace\"\n").unwrap())
        .log_sink(|_record| {})
        .profile()
        .build()
        .unwrap();
    let mut plugin = host.load_binary("logs", LOGS.as_bytes()).unwrap();
    plugin
        .call("emit", &[Val::Enum("warn".into()), Val::U32(3)])
        .unwrap();

    let profile = plugin.profile().unwrap();
    let row = profile
        .functions
        .iter()
        .find(|row| row.kind == FunctionKind::Import)
        .unwrap_or_else(|| panic!("no import row in {profile:?}"));
    assert_eq!(row.interface, "wasi:logging/logging@0.1.0-draft");
    assert_eq!(row.func, "log");
    assert_eq!(row.calls, 3);
}

// ---------------------------------------------------------------------------
// Refusals and the sampled profile
// ---------------------------------------------------------------------------

#[test]
fn profiling_and_recording_together_are_refused() {
    use std::sync::Arc;
    use watoots::{TraceEvent, TraceHook};

    struct Silent;
    impl TraceHook for Silent {
        fn on_event(&self, _event: &TraceEvent<'_>) {}
    }

    let err = Host::builder()
        .profile()
        .trace_hook(Arc::new(Silent) as Arc<dyn TraceHook>)
        .build()
        .expect_err("a trace recorded under a profiler does not reproduce");
    assert_eq!(err.kind(), ErrorKind::InvalidArgument);
    assert!(err.message().contains("profiling"), "{}", err.message());

    // And in the other order, since a builder is a bag rather than a sequence.
    let err = Host::builder()
        .trace_hook(Arc::new(Silent) as Arc<dyn TraceHook>)
        .profile()
        .build()
        .expect_err("order must not matter");
    assert_eq!(err.kind(), ErrorKind::InvalidArgument);
}

#[test]
fn a_firefox_profile_is_written_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("guest.json");

    let host = Host::builder()
        .profile_guest_samples(Duration::from_millis(1))
        .build()
        .unwrap();
    let mut plugin = host.load_binary("busy", BUSY.as_bytes()).unwrap();
    plugin.call("spin", &[Val::S32(20_000_000)]).unwrap();

    plugin.write_guest_profile(&path).unwrap();
    let json = std::fs::read_to_string(&path).unwrap();
    assert!(
        json.starts_with('{'),
        "the processed profile format is JSON"
    );
    assert!(
        json.contains("\"threads\""),
        "a processed profile has threads"
    );

    // The profiler is consumed by writing it, which the message has to say
    // rather than leaving a second call to produce an empty file.
    let err = plugin.write_guest_profile(&path).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidArgument);
}

#[test]
fn asking_for_a_firefox_profile_that_was_never_sampled_fails() {
    let dir = tempfile::tempdir().unwrap();
    let host = profiled("");
    let mut plugin = host.load_binary("busy", BUSY.as_bytes()).unwrap();
    let err = plugin
        .write_guest_profile(dir.path().join("guest.json"))
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidArgument);
    assert!(err.message().contains("profile_guest_samples"));
}

#[test]
fn a_component_loaded_from_the_cwasm_cache_can_still_be_sampled() {
    // GuestProfiler needs the component's core modules. A deserialized
    // component carries them, so the precompile cache needs no special case —
    // this is the test that says so rather than the ADR's guess that it might.
    let dir = tempfile::tempdir().unwrap();
    let build = || {
        Host::builder()
            .cache_dir(dir.path())
            .profile_guest_samples(Duration::from_millis(1))
            .build()
            .unwrap()
    };

    // First load fills the cache; the second reads it back.
    drop(build().load_binary("busy", BUSY.as_bytes()).unwrap());
    let entries = std::fs::read_dir(dir.path()).unwrap().count();
    assert_eq!(entries, 1, "the first load should have written a .cwasm");

    let host = build();
    let mut plugin = host.load_binary("busy", BUSY.as_bytes()).unwrap();
    plugin.call("spin", &[Val::S32(20_000_000)]).unwrap();
    assert!(plugin.profile().unwrap().guest_nanos > 0);

    let path = dir.path().join("guest.json");
    plugin.write_guest_profile(&path).unwrap();
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .contains("\"threads\"")
    );
}

#[test]
fn nothing_is_profiled_when_profiling_is_off() {
    // The other half of "opt-in": a host built without it behaves exactly as
    // before, including still stopping a runaway guest.
    let host = Host::builder()
        .manifest(Manifest::parse("[limits]\nfuel = 100_000\n").unwrap())
        .build()
        .unwrap();
    let mut plugin = host.load_binary("spinner", SPINNER.as_bytes()).unwrap();
    assert_eq!(
        plugin.call("spin", &[]).unwrap_err().kind(),
        ErrorKind::LimitExceeded
    );
    assert!(plugin.profile().is_err());
}
