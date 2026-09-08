//! The audit trail: what a plugin was permitted to do, and what it was refused.
//!
//! The components are WAT, so the suite still needs no guest toolchain — the
//! same reason `logging.rs` builds its guest by hand.
//!
//! Two properties get more attention here than anywhere else, because they are
//! the ones ADR-0011 is built around and the ones a reader would otherwise have
//! to take on trust:
//!
//! - **a denied load names the import that decided it**, since that is the event
//!   somebody reads after an incident; and
//! - **a suppressed log line does not carry the line**, which is asserted as an
//!   absence rather than inferred from a variant's field list.

use std::sync::{Arc, Mutex};

use watoots::{
    AuditEvent, AuditHook, Ceiling, ErrorKind, Host, HostBuilder, LogLevel, Manifest, Verdict,
};

// ---------------------------------------------------------------------------
// Components
// ---------------------------------------------------------------------------

/// Declares `wasi:logging` and nothing else — enough for the grant check.
const DECLARES_LOGGING: &str = r#"
(component
  (import "wasi:logging/logging@0.1.0-draft" (instance (export "log" (func))))
)
"#;

/// Calls `log`, `count` times, at the level it is handed. Lifted from
/// `logging.rs`: "ctx" and "hello" are two data segments, so every message
/// charges exactly 8 bytes against `limits.log_bytes` and the *message text* is
/// a known string a test can go looking for in the audit trail.
const LOGS: &str = r#"
(component
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

/// Wants the network, which no manifest here grants.
const WANTS_NETWORK: &str = r#"
(component
  (import "wasi:sockets/tcp@0.2.6" (instance (export "connect" (func))))
)
"#;

/// Imports an interface that holds only type definitions: nothing callable, so
/// nothing to grant.
const WANTS_TYPES_ONLY: &str = r#"
(component
  (import "watoots:example/types@0.1.0" (instance))
)
"#;

/// Calls out to an interface the application serves.
const CALLS_OUT: &str = r#"
(component
  (import "app:demo/sink" (instance $host
    (export "tick" (func))))
  (core module $m
    (import "host" "tick" (func $tick))
    (func (export "run") (call $tick)))
  (core func $tick (canon lower (func $host "tick")))
  (core instance $hi (instantiate $m
    (with "host" (instance (export "tick" (func $tick))))))
  (func $run (canon lift (core func $hi "run")))
  (export "run" (func $run))
)
"#;

/// Loops forever. Only a ceiling can stop it.
const SPINNER: &str = r#"
(component
  (core module $m
    (func (export "spin") (loop $l br $l)))
  (core instance $i (instantiate $m))
  (func $spin (canon lift (core func $i "spin")))
  (export "spin" (func $spin))
)
"#;

/// Grows its linear memory by however many pages it is asked for.
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

/// A counter with both reload state hooks, and a second build whose
/// `save-state` returns a different type — a reload the host has to refuse
/// after the replacement was already compiled and granted.
const COUNTER: &str = r#"
(component
  (core module $m
    (global $n (mut i32) (i32.const 0))
    (func (export "get") (result i32) (global.get $n))
    (func (export "save") (result i32) (global.get $n))
    (func (export "restore") (param i32) (global.set $n (local.get 0))))
  (core instance $i (instantiate $m))
  (func $get (result u32) (canon lift (core func $i "get")))
  (func $save (result u32) (canon lift (core func $i "save")))
  (func $restore (param "state" u32) (canon lift (core func $i "restore")))
  (export "get" (func $get))
  (export "save-state" (func $save))
  (export "restore-state" (func $restore))
)
"#;

/// The same world, but `restore-state` takes a float: the state cannot cross.
const COUNTER_WRONG_STATE: &str = r#"
(component
  (core module $m
    (global $n (mut i32) (i32.const 0))
    (func (export "get") (result i32) (global.get $n))
    (func (export "save") (result i32) (global.get $n))
    (func (export "restore") (param f64)))
  (core instance $i (instantiate $m))
  (func $get (result u32) (canon lift (core func $i "get")))
  (func $save (result u32) (canon lift (core func $i "save")))
  (func $restore (param "state" f64) (canon lift (core func $i "restore")))
  (export "get" (func $get))
  (export "save-state" (func $save))
  (export "restore-state" (func $restore))
)
"#;

// ---------------------------------------------------------------------------
// The collector
// ---------------------------------------------------------------------------

/// Everything a hook was told, kept twice over.
///
/// `lines` is the rendering — the same string the CLI prints and the C surface
/// hands over — so asserting on it pins the rendering as well as the decision.
/// `facts` is the structured half, reduced to what each test needs to see
/// without matching on a borrowed event outside the hook.
#[derive(Default)]
struct Audit {
    lines: Mutex<Vec<String>>,
    facts: Mutex<Vec<Fact>>,
}

/// The fields a test asserts on, lifted out of the borrowed event.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Fact {
    Loaded {
        plugin: String,
        sha256: String,
        imports: usize,
    },
    LoadRefused {
        plugin: String,
        import: Option<String>,
    },
    LoadedUnverified {
        plugin: String,
    },
    Import {
        import: String,
        verdict: Verdict,
    },
    Reloaded {
        reloads: u64,
    },
    ReloadRefused {
        import: Option<String>,
    },
    LogAdmitted {
        level: LogLevel,
    },
    LogSuppressed {
        level: Option<LogLevel>,
        ceiling: LogLevel,
    },
    CeilingSpent {
        limit: Ceiling,
    },
}

impl AuditHook for Audit {
    fn on_event(&self, event: &AuditEvent<'_>) {
        self.lines.lock().unwrap().push(event.to_string());
        let fact = match event {
            AuditEvent::Loaded {
                plugin,
                sha256,
                imports,
            } => Fact::Loaded {
                plugin: (*plugin).to_string(),
                sha256: (*sha256).to_string(),
                imports: *imports,
            },
            AuditEvent::LoadRefused { plugin, import, .. } => Fact::LoadRefused {
                plugin: (*plugin).to_string(),
                import: import.map(str::to_string),
            },
            AuditEvent::LoadedUnverified { plugin, .. } => Fact::LoadedUnverified {
                plugin: (*plugin).to_string(),
            },
            AuditEvent::ImportDecided {
                import, verdict, ..
            } => Fact::Import {
                import: (*import).to_string(),
                verdict: *verdict,
            },
            AuditEvent::Reloaded { reloads, .. } => Fact::Reloaded { reloads: *reloads },
            AuditEvent::ReloadRefused { import, .. } => Fact::ReloadRefused {
                import: import.map(str::to_string),
            },
            AuditEvent::LogAdmitted { level, .. } => Fact::LogAdmitted { level: *level },
            AuditEvent::LogSuppressed { level, ceiling, .. } => Fact::LogSuppressed {
                level: *level,
                ceiling: *ceiling,
            },
            AuditEvent::CeilingSpent { limit, .. } => Fact::CeilingSpent { limit: *limit },
        };
        self.facts.lock().unwrap().push(fact);
    }
}

impl Audit {
    fn lines(&self) -> Vec<String> {
        self.lines.lock().unwrap().clone()
    }

    fn facts(&self) -> Vec<Fact> {
        self.facts.lock().unwrap().clone()
    }

    /// How many events of one kind arrived, by the name that leads its line.
    fn count(&self, kind: &str) -> usize {
        self.lines()
            .iter()
            .filter(|line| line.starts_with(&format!("{kind} ")))
            .count()
    }

    /// The whole trail as one blob, for `assert!(!contains)` and for messages.
    fn joined(&self) -> String {
        self.lines().join("\n")
    }
}

/// A host with an audit hook and whatever manifest the test needs.
fn audited(manifest_toml: &str) -> (Host, Arc<Audit>) {
    audited_with(manifest_toml, |builder| builder)
}

fn audited_with(
    manifest_toml: &str,
    edit: impl FnOnce(HostBuilder) -> HostBuilder,
) -> (Host, Arc<Audit>) {
    let audit = Arc::new(Audit::default());
    let builder = Host::builder()
        .manifest(Manifest::parse(manifest_toml).unwrap())
        .audit_hook(Arc::clone(&audit) as Arc<dyn AuditHook>);
    (edit(builder).build().unwrap(), audit)
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

#[test]
fn a_load_is_recorded_with_the_bytes_it_ran() {
    let (host, audit) = audited("[permissions]\nlogging = \"trace\"\n");
    host.load_binary("talker", LOGS.as_bytes()).unwrap();

    assert_eq!(audit.count("loaded"), 1, "{}", audit.joined());
    let Some(Fact::Loaded {
        plugin,
        sha256,
        imports,
    }) = audit
        .facts()
        .into_iter()
        .find(|fact| matches!(fact, Fact::Loaded { .. }))
    else {
        panic!("no load event: {}", audit.joined());
    };

    assert_eq!(plugin, "talker");
    assert_eq!(imports, 1);
    // "From what bytes" is the question the plugin's name cannot answer, and
    // the same digest a recorded trace names the component by.
    assert_eq!(sha256.len(), 64, "{sha256}");
    assert!(sha256.chars().all(|c| c.is_ascii_hexdigit()), "{sha256}");
}

/// The event somebody reads after an incident.
#[test]
fn a_denied_load_emits_the_denial_and_names_the_deciding_import() {
    let (host, audit) = audited("");
    let err = host
        .load_binary("net", WANTS_NETWORK.as_bytes())
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::PermissionDenied);

    assert_eq!(audit.count("load-refused"), 1, "{}", audit.joined());
    assert_eq!(audit.count("loaded"), 0, "{}", audit.joined());

    // Not "a load was refused" — *which import* refused it. A refusal that only
    // says no sends whoever reads it back to the component to work out why.
    assert!(
        audit.facts().contains(&Fact::LoadRefused {
            plugin: "net".to_string(),
            import: Some("wasi:sockets/tcp@0.2.6".to_string()),
        }),
        "{}",
        audit.joined()
    );

    // And the rendered line carries both the import and the key to edit.
    let line = audit
        .lines()
        .into_iter()
        .find(|line| line.starts_with("load-refused "))
        .unwrap();
    assert!(line.contains("import=wasi:sockets/tcp@0.2.6"), "{line}");
    assert!(line.contains("grant=permissions.net"), "{line}");
}

#[test]
fn every_import_gets_a_verdict_and_a_granted_one_says_so() {
    let (host, audit) = audited("[permissions]\nlogging = \"warn\"\n");
    host.load_binary("chatty", DECLARES_LOGGING.as_bytes())
        .unwrap();

    assert_eq!(
        audit.facts(),
        vec![
            Fact::Import {
                import: "wasi:logging/logging@0.1.0-draft".to_string(),
                verdict: Verdict::Granted,
            },
            Fact::Loaded {
                plugin: "chatty".to_string(),
                sha256: audit
                    .facts()
                    .into_iter()
                    .find_map(|fact| match fact {
                        Fact::Loaded { sha256, .. } => Some(sha256),
                        _ => None,
                    })
                    .unwrap(),
                imports: 1,
            },
            // This manifest lists no signature keys, so the trail says the
            // plugin ran with nobody having checked who wrote it. It follows
            // `Loaded` rather than replacing it: the load did succeed, and what
            // is missing is the publisher check, not the load.
            Fact::LoadedUnverified {
                plugin: "chatty".to_string(),
            },
        ],
        "{}",
        audit.joined()
    );
    // The verdicts come first, so a refusal arrives already explained.
    assert!(
        audit.lines()[0].starts_with("import "),
        "{}",
        audit.joined()
    );
}

#[test]
fn a_denied_import_is_reported_as_denied_before_the_refusal() {
    let (host, audit) = audited("");
    host.load_binary("quiet", DECLARES_LOGGING.as_bytes())
        .unwrap_err();

    assert!(
        audit.facts().contains(&Fact::Import {
            import: "wasi:logging/logging@0.1.0-draft".to_string(),
            verdict: Verdict::Denied,
        }),
        "{}",
        audit.joined()
    );
    assert!(
        audit.lines()[0].starts_with("import "),
        "the verdict explains the refusal, so it comes first: {}",
        audit.joined()
    );
}

#[test]
fn an_interface_the_application_serves_is_not_reported_as_a_grant() {
    // Three of the four verdicts read as "allowed" in a yes/no column, and only
    // one of them is a capability the manifest handed out. An auditor has to be
    // able to tell them apart.
    let (host, audit) = audited_with("", |builder| {
        builder.host_func("app:demo/sink", "tick", |_call| Ok(Vec::new()))
    });
    host.load_binary("caller", CALLS_OUT.as_bytes()).unwrap();

    assert!(
        audit.facts().contains(&Fact::Import {
            import: "app:demo/sink".to_string(),
            verdict: Verdict::HostProvided,
        }),
        "{}",
        audit.joined()
    );
}

#[test]
fn a_types_only_import_is_reported_as_types_only_and_not_as_a_grant() {
    // Nothing callable is in there, so there was nothing to grant. An import
    // list that showed this as "allowed" would be telling an auditor that a
    // manifest handed something out, when the manifest was never asked.
    let (host, audit) = audited("");
    host.load_binary("types", WANTS_TYPES_ONLY.as_bytes())
        .unwrap();

    assert!(
        audit.facts().contains(&Fact::Import {
            import: "watoots:example/types@0.1.0".to_string(),
            verdict: Verdict::TypesOnly,
        }),
        "{}",
        audit.joined()
    );
}

#[test]
fn a_refusal_that_no_import_decided_names_none() {
    // Bytes that are not a component are refused before there is anything to
    // classify. Naming an import there would be inventing one.
    let (host, audit) = audited("");
    let err = host.load_binary("junk", b"not a component").unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Load);

    assert_eq!(
        audit.facts(),
        vec![Fact::LoadRefused {
            plugin: "junk".to_string(),
            import: None,
        }],
        "{}",
        audit.joined()
    );
}

// ---------------------------------------------------------------------------
// Logging
// ---------------------------------------------------------------------------

#[test]
fn an_admitted_log_line_is_recorded_at_its_level() {
    let (host, audit) = audited("[permissions]\nlogging = \"trace\"\n");
    let mut plugin = host.load_binary("talker", LOGS.as_bytes()).unwrap();

    plugin.call_wave("emit", &["info", "2"]).unwrap();

    assert_eq!(audit.count("log-admitted"), 2, "{}", audit.joined());
    assert_eq!(audit.count("log-suppressed"), 0, "{}", audit.joined());
    assert!(
        audit.facts().contains(&Fact::LogAdmitted {
            level: LogLevel::Info
        }),
        "{}",
        audit.joined()
    );
}

/// ADR-0011's constraint, made executable.
#[test]
fn a_suppressed_log_line_is_recorded_and_never_carries_the_message() {
    let (host, audit) = audited("[permissions]\nlogging = \"warn\"\n");
    let mut plugin = host.load_binary("talker", LOGS.as_bytes()).unwrap();

    plugin.call_wave("emit", &["trace", "3"]).unwrap();

    assert_eq!(audit.count("log-suppressed"), 3, "{}", audit.joined());
    assert!(
        audit.facts().contains(&Fact::LogSuppressed {
            level: Some(LogLevel::Trace),
            ceiling: LogLevel::Warn,
        }),
        "{}",
        audit.joined()
    );

    // The assertion this whole hook exists to be able to make. `LOGS` logs the
    // context "ctx" and the message "hello"; neither may appear anywhere in the
    // trail, because an audit line has to be safe to keep and to paste into an
    // issue, and a trace is the thing that is not.
    let trail = audit.joined();
    assert!(!trail.contains("hello"), "the message body leaked: {trail}");
    assert!(!trail.contains("ctx"), "the context leaked: {trail}");
}

#[test]
fn the_two_are_distinguishable_which_is_the_whole_point() {
    // "The plugin said nothing" and "the plugin was not allowed to say it" mean
    // opposite things, and until this hook existed they looked identical.
    let (host, audit) = audited("[permissions]\nlogging = \"error\"\n");
    let mut plugin = host.load_binary("talker", LOGS.as_bytes()).unwrap();

    plugin.call_wave("emit", &["warn", "1"]).unwrap();
    plugin.call_wave("emit", &["error", "1"]).unwrap();

    assert_eq!(audit.count("log-suppressed"), 1, "{}", audit.joined());
    assert_eq!(audit.count("log-admitted"), 1, "{}", audit.joined());
}

#[test]
fn a_grant_with_no_sink_still_records_the_admission() {
    // The decision is the manifest's; whether anybody was listening is not.
    let (host, audit) = audited("[permissions]\nlogging = \"trace\"\n");
    let mut plugin = host.load_binary("mute", LOGS.as_bytes()).unwrap();

    plugin.call_wave("emit", &["error", "2"]).unwrap();
    assert_eq!(audit.count("log-admitted"), 2, "{}", audit.joined());
}

// ---------------------------------------------------------------------------
// Ceilings
// ---------------------------------------------------------------------------

/// Only the ceiling events, so a test can say "exactly this one, and no other".
fn ceilings(audit: &Audit) -> Vec<Fact> {
    audit
        .facts()
        .into_iter()
        .filter(|fact| matches!(fact, Fact::CeilingSpent { .. }))
        .collect()
}

#[test]
fn a_spent_fuel_ceiling_names_the_limit() {
    let (host, audit) = audited("[limits]\nfuel = 100_000\n");
    let mut plugin = host.load_binary("spinner", SPINNER.as_bytes()).unwrap();

    let err = plugin.call("spin", &[]).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::LimitExceeded);

    assert_eq!(
        ceilings(&audit),
        vec![Fact::CeilingSpent {
            limit: Ceiling::Fuel
        }],
        "exactly one ceiling event, and it is fuel: {}",
        audit.joined()
    );
    assert!(
        audit.joined().contains("limit=limits.fuel"),
        "{}",
        audit.joined()
    );
}

#[test]
fn a_spent_deadline_names_the_timeout() {
    let (host, audit) = audited("[limits]\ntimeout = \"20ms\"\n");
    let mut plugin = host.load_binary("spinner", SPINNER.as_bytes()).unwrap();

    plugin.call("spin", &[]).unwrap_err();
    assert!(
        audit.facts().contains(&Fact::CeilingSpent {
            limit: Ceiling::Timeout
        }),
        "{}",
        audit.joined()
    );
}

#[test]
fn a_refused_memory_growth_is_recorded_although_the_guest_only_sees_minus_one() {
    // The one ceiling that produces no error at all: `memory.grow` answers -1
    // and the call succeeds. Without this event, "the plugin hit its memory
    // ceiling" reaches nobody.
    let (host, audit) = audited("[limits]\nmemory = \"1MiB\"\n");
    let mut plugin = host.load_binary("grower", GROWER.as_bytes()).unwrap();

    let results = plugin
        .call_wave("grow", &["1000"])
        .expect("a refused growth is not an error");
    assert_eq!(results, vec!["-1".to_string()]);

    assert!(
        audit.facts().contains(&Fact::CeilingSpent {
            limit: Ceiling::Memory
        }),
        "{}",
        audit.joined()
    );
}

#[test]
fn a_spent_log_message_budget_names_that_limit_and_not_fuel() {
    let (host, audit) =
        audited("[permissions]\nlogging = \"trace\"\n\n[limits]\nlog_messages = 2\n");
    let mut plugin = host.load_binary("firehose", LOGS.as_bytes()).unwrap();

    let err = plugin.call_wave("emit", &["info", "5"]).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::LimitExceeded);

    // The shim is the only thing that knows *which* of the two log budgets went,
    // so the call path must not report it a second time as something else.
    assert_eq!(
        ceilings(&audit),
        vec![Fact::CeilingSpent {
            limit: Ceiling::LogMessages
        }],
        "{}",
        audit.joined()
    );
}

#[test]
fn a_spent_log_byte_budget_names_that_limit() {
    // Each message is "ctx" + "hello" = 8 bytes, so 12 admits exactly one.
    let (host, audit) = audited("[permissions]\nlogging = \"trace\"\n\n[limits]\nlog_bytes = 12\n");
    let mut plugin = host.load_binary("firehose", LOGS.as_bytes()).unwrap();

    plugin.call_wave("emit", &["info", "4"]).unwrap_err();
    assert!(
        audit.facts().contains(&Fact::CeilingSpent {
            limit: Ceiling::LogBytes
        }),
        "{}",
        audit.joined()
    );
}

// ---------------------------------------------------------------------------
// Reload
// ---------------------------------------------------------------------------

#[test]
fn a_reload_is_recorded_as_a_reload_rather_than_a_second_load() {
    let (host, audit) = audited("");
    let mut plugin = host.load_binary("counter", COUNTER.as_bytes()).unwrap();
    assert_eq!(audit.count("loaded"), 1);

    plugin.reload(COUNTER.as_bytes()).unwrap();

    assert_eq!(audit.count("reloaded"), 1, "{}", audit.joined());
    assert_eq!(
        audit.count("loaded"),
        1,
        "a reload is not a second load: {}",
        audit.joined()
    );
    assert!(
        audit.facts().contains(&Fact::Reloaded { reloads: 1 }),
        "{}",
        audit.joined()
    );
}

#[test]
fn a_reload_refused_for_an_ungranted_import_emits_a_refusal_that_names_it() {
    let (host, audit) = audited("");
    let mut plugin = host.load_binary("counter", COUNTER.as_bytes()).unwrap();

    let err = plugin.reload(WANTS_NETWORK.as_bytes()).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::PermissionDenied);

    assert_eq!(audit.count("reload-refused"), 1, "{}", audit.joined());
    assert_eq!(audit.count("reloaded"), 0, "{}", audit.joined());
    // New bytes must not acquire a capability by arriving as an update, and the
    // refusal names the import exactly as a first load's would.
    assert!(
        audit.facts().contains(&Fact::ReloadRefused {
            import: Some("wasi:sockets/tcp@0.2.6".to_string()),
        }),
        "{}",
        audit.joined()
    );
    // A refused reload is not a refused *load*: the plugin is still running.
    assert_eq!(audit.count("load-refused"), 0, "{}", audit.joined());
}

#[test]
fn a_reload_refused_after_the_replacement_was_built_still_emits_a_refusal() {
    // The replacement compiles and is granted; the state cannot cross. That is
    // still a reload that did not happen, and it has to be recorded as one --
    // with no deciding import, because none decided it.
    let (host, audit) = audited("");
    let mut plugin = host.load_binary("counter", COUNTER.as_bytes()).unwrap();

    let err = plugin.reload(COUNTER_WRONG_STATE.as_bytes()).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidArgument);

    assert_eq!(audit.count("reloaded"), 0, "{}", audit.joined());
    assert!(
        audit
            .facts()
            .contains(&Fact::ReloadRefused { import: None }),
        "{}",
        audit.joined()
    );
}

// ---------------------------------------------------------------------------
// Every kind, once
// ---------------------------------------------------------------------------

#[test]
fn every_decision_kind_is_emitted_exactly_once_when_it_happens_once() {
    // One message per call, and a ceiling of `warn`: the second call therefore
    // gets one suppression and then trips the budget, which is one of each.
    let (host, audit) =
        audited("[permissions]\nlogging = \"warn\"\n\n[limits]\nlog_messages = 1\n");
    let mut plugin = host.load_binary("talker", LOGS.as_bytes()).unwrap();

    plugin.call_wave("emit", &["error", "1"]).unwrap(); // admitted
    plugin.call_wave("emit", &["trace", "2"]).unwrap_err(); // suppressed, then ceiling
    plugin.reload(LOGS.as_bytes()).unwrap(); // reloaded
    plugin.reload(WANTS_NETWORK.as_bytes()).unwrap_err(); // reload-refused

    for (kind, expected) in [
        ("loaded", 1),
        ("log-admitted", 1),
        ("log-suppressed", 1),
        ("ceiling-spent", 1),
        ("reloaded", 1),
        ("reload-refused", 1),
    ] {
        assert_eq!(audit.count(kind), expected, "{kind}: {}", audit.joined());
    }

    // One verdict per import per grant check, and the check runs three times:
    // the load and the two reloads.
    assert_eq!(audit.count("import"), 3, "{}", audit.joined());

    // The remaining kind needs a plugin that never starts, so it gets its own
    // host rather than a contrived sixth call here.
    let (refuser, refused) = audited("");
    refuser
        .load_binary("net", WANTS_NETWORK.as_bytes())
        .unwrap_err();
    assert_eq!(refused.count("load-refused"), 1, "{}", refused.joined());
}

// ---------------------------------------------------------------------------
// Off by default
// ---------------------------------------------------------------------------

#[test]
fn no_hook_installed_means_no_behaviour_change() {
    // Everything the hook observes is a decision that was already being made,
    // so a host without one must behave identically -- same values, same
    // counters, same error kinds and the same messages. The cost side of the
    // claim is structural: every emission sits behind `if let Some(hook)`, and
    // the component digest, the only work the trail adds, is computed nowhere
    // else.
    let manifest = "[permissions]\nlogging = \"warn\"\n\n[limits]\nlog_messages = 2\n";

    let run = |host: &Host| {
        let mut plugin = host.load_binary("talker", LOGS.as_bytes()).unwrap();
        plugin.call_wave("emit", &["error", "1"]).unwrap();
        plugin.call_wave("emit", &["trace", "1"]).unwrap();
        let err = plugin.call_wave("emit", &["error", "9"]).unwrap_err();
        let denial = host
            .load_binary("net", WANTS_NETWORK.as_bytes())
            .unwrap_err();
        (
            plugin.stats(),
            err.kind(),
            err.message().to_string(),
            denial.kind(),
            denial.message().to_string(),
        )
    };

    let plain = Host::builder()
        .manifest(Manifest::parse(manifest).unwrap())
        .build()
        .unwrap();
    let (audited_host, audit) = audited(manifest);

    assert_eq!(run(&plain), run(&audited_host));
    assert!(
        !audit.lines().is_empty(),
        "the audited run has to have actually observed something"
    );
}

#[test]
fn inspecting_a_component_decides_nothing_and_records_nothing() {
    // `Host::inspect` compiles and classifies but instantiates nothing, so it
    // is a question rather than a decision. Recording it would fill the trail
    // with loads that never happened.
    let (host, audit) = audited("");
    host.inspect(WANTS_NETWORK.as_bytes()).unwrap();
    assert!(audit.lines().is_empty(), "{}", audit.joined());
}
