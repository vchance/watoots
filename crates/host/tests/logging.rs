//! `wasi:logging` as a granted capability: the grant, the level ceiling, and
//! the volume ceiling.
//!
//! The components are WAT, so the suite still needs no guest toolchain. What
//! `LOGS` below is doing by hand is what `wit-bindgen` would emit for
//! `wasi:logging@0.1.0-draft`: an imported instance holding `enum level` and
//! `log`, lowered into a core module that calls it with pointers into its own
//! memory.

use std::sync::{Arc, Mutex};

use watoots::{ErrorKind, Host, LogLevel, LogRecord, Manifest, Requirement};

/// Declares the import and nothing else — enough for the grant check.
const DECLARES_LOGGING: &str = r#"
(component
  (import "wasi:logging/logging@0.1.0-draft" (instance (export "log" (func))))
)
"#;

/// Actually calls `log`, `count` times, at the level it is handed.
///
/// `emit` re-exports the imported `level` enum so a test can name a level in
/// WAVE rather than by discriminant, and one component can exercise all six.
/// The strings are two data segments: "ctx" (3 bytes) and "hello" (5), so each
/// message charges exactly 8 bytes against `limits.log_bytes`.
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

/// Collects what the sink was handed, so a test can assert on it.
#[derive(Default)]
struct Collected(Mutex<Vec<(LogLevel, String, String)>>);

impl Collected {
    fn take(&self) -> Vec<(LogLevel, String, String)> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }

    fn levels(&self) -> Vec<LogLevel> {
        self.0.lock().unwrap().iter().map(|entry| entry.0).collect()
    }
}

/// A host with a manifest and a sink that remembers everything.
fn host_with_sink(manifest_toml: &str) -> (Host, Arc<Collected>) {
    let collected = Arc::new(Collected::default());
    let sink = Arc::clone(&collected);
    let host = Host::builder()
        .manifest(Manifest::parse(manifest_toml).unwrap())
        .log_sink(move |record: &LogRecord<'_>| {
            sink.0.lock().unwrap().push((
                record.level(),
                record.context().to_string(),
                record.message().to_string(),
            ));
        })
        .build()
        .unwrap();
    (host, collected)
}

#[test]
fn logging_is_refused_at_load_time_when_the_manifest_says_nothing() {
    // The central thesis, applied to logging: you find out that a plugin wants
    // to talk when you install it, not when it first says something.
    let (host, _) = host_with_sink("");
    let err = host
        .load_binary("quiet", DECLARES_LOGGING.as_bytes())
        .unwrap_err();

    assert_eq!(err.kind(), ErrorKind::PermissionDenied);
    assert!(
        err.message().contains("wasi:logging/logging"),
        "{}",
        err.message()
    );
    assert!(
        err.message().contains("permissions.logging"),
        "a denial should say how to fix it, got: {}",
        err.message()
    );
}

#[test]
fn logging_loads_when_the_manifest_grants_a_level() {
    let (host, _) = host_with_sink("[permissions]\nlogging = \"trace\"\n");
    let plugin = host
        .load_binary("chatty", DECLARES_LOGGING.as_bytes())
        .unwrap();

    let report = plugin.grants();
    assert!(report.is_satisfied(), "{}", report.describe());
    assert_eq!(report.decisions[0].requirement, Requirement::Logging);
}

#[test]
fn a_granted_plugin_reaches_the_application_sink() {
    let (host, collected) = host_with_sink("[permissions]\nlogging = \"trace\"\n");
    let mut plugin = host.load_binary("talker", LOGS.as_bytes()).unwrap();

    plugin
        .call_wave("emit", &[LogLevel::Info.as_wit_name(), "2"])
        .unwrap();

    assert_eq!(
        collected.take(),
        vec![
            (LogLevel::Info, "ctx".to_string(), "hello".to_string()),
            (LogLevel::Info, "ctx".to_string(), "hello".to_string()),
        ]
    );
}

#[test]
fn the_level_ceiling_filters() {
    let (host, collected) = host_with_sink("[permissions]\nlogging = \"warn\"\n");
    let mut plugin = host.load_binary("talker", LOGS.as_bytes()).unwrap();

    for level in LogLevel::ALL {
        plugin
            .call_wave("emit", &[level.as_wit_name(), "1"])
            .unwrap();
    }

    // Every level was emitted; only the ones at or above the ceiling arrived.
    assert_eq!(
        collected.levels(),
        vec![LogLevel::Warn, LogLevel::Error, LogLevel::Critical]
    );
}

#[test]
fn the_strictest_ceiling_still_admits_its_own_level() {
    let (host, collected) = host_with_sink("[permissions]\nlogging = \"critical\"\n");
    let mut plugin = host.load_binary("talker", LOGS.as_bytes()).unwrap();

    plugin.call_wave("emit", &["error", "3"]).unwrap();
    assert!(collected.levels().is_empty());

    plugin.call_wave("emit", &["critical", "1"]).unwrap();
    assert_eq!(collected.levels(), vec![LogLevel::Critical]);
}

#[test]
fn the_message_ceiling_trips_and_reports_a_limit_not_a_trap() {
    let (host, collected) =
        host_with_sink("[permissions]\nlogging = \"trace\"\n\n[limits]\nlog_messages = 2\n");
    let mut plugin = host.load_binary("firehose", LOGS.as_bytes()).unwrap();

    let err = plugin.call_wave("emit", &["info", "5"]).unwrap_err();

    assert_eq!(err.kind(), ErrorKind::LimitExceeded);
    assert!(
        err.message().contains("limits.log_messages"),
        "{}",
        err.message()
    );
    // The two that fit still reached the sink; the third is what stopped it.
    assert_eq!(collected.levels().len(), 2);
}

#[test]
fn the_byte_ceiling_trips() {
    // Each message is "ctx" + "hello" = 8 bytes, so 12 admits exactly one.
    let (host, collected) =
        host_with_sink("[permissions]\nlogging = \"trace\"\n\n[limits]\nlog_bytes = 12\n");
    let mut plugin = host.load_binary("firehose", LOGS.as_bytes()).unwrap();

    let err = plugin.call_wave("emit", &["info", "4"]).unwrap_err();

    assert_eq!(err.kind(), ErrorKind::LimitExceeded);
    assert!(
        err.message().contains("limits.log_bytes"),
        "{}",
        err.message()
    );
    assert_eq!(collected.levels().len(), 1);
}

#[test]
fn the_volume_ceiling_is_per_call_and_re_arms() {
    let (host, collected) =
        host_with_sink("[permissions]\nlogging = \"trace\"\n\n[limits]\nlog_messages = 2\n");
    let mut plugin = host.load_binary("talker", LOGS.as_bytes()).unwrap();

    for _ in 0..3 {
        plugin.call_wave("emit", &["info", "2"]).unwrap();
    }
    assert_eq!(collected.levels().len(), 6);
}

#[test]
fn a_filtered_message_still_costs_its_budget() {
    // Otherwise `logging = "critical"` would be a licence to push unbounded
    // bytes across the boundary at `trace`: the host has already paid to lift
    // the string out of guest memory by the time the level is readable.
    let (host, collected) =
        host_with_sink("[permissions]\nlogging = \"critical\"\n\n[limits]\nlog_messages = 2\n");
    let mut plugin = host.load_binary("sneaky", LOGS.as_bytes()).unwrap();

    let err = plugin.call_wave("emit", &["trace", "9"]).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::LimitExceeded);
    assert!(collected.levels().is_empty());
}

#[test]
fn a_grant_with_no_sink_links_and_discards() {
    // An application that grants logging and registers nothing is almost
    // certainly wrong, but refusing to load is the wrong correction: the
    // manifest is what decides admission.
    let host = Host::builder()
        .manifest(Manifest::parse("[permissions]\nlogging = \"trace\"\n").unwrap())
        .build()
        .unwrap();
    let mut plugin = host.load_binary("mute", LOGS.as_bytes()).unwrap();
    plugin.call_wave("emit", &["error", "3"]).unwrap();
}

#[test]
fn an_unversioned_import_resolves_too() {
    // A guest built from a vendored WIT that dropped the package version
    // imports the bare name. Both spellings are registered.
    const UNVERSIONED: &str = r#"
(component
  (import "wasi:logging/logging" (instance (export "log" (func))))
)
"#;
    let (host, _) = host_with_sink("[permissions]\nlogging = \"trace\"\n");
    let plugin = host.load_binary("bare", UNVERSIONED.as_bytes()).unwrap();
    assert!(plugin.grants().is_satisfied());
}
