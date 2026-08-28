//! Load-time behaviour: the import-intersection check, and what a denial says.
//!
//! Components are built from WAT so these tests need no guest toolchain and
//! run in milliseconds. What matters here is a component's *import list*, and
//! WAT states that directly.

use watoots::{ErrorKind, Host, Manifest, Requirement};

/// A component that imports nothing and exports one function.
const SELF_CONTAINED: &str = r#"
(component
  (core module $m
    (func (export "answer") (result i32) i32.const 42))
  (core instance $i (instantiate $m))
  (func $answer (result s32) (canon lift (core func $i "answer")))
  (export "answer" (func $answer))
)
"#;

// Each imported instance below declares a function. An instance with no
// exports at all is a *type-only* import and conveys no capability, so it would
// be granted — see `a_type_only_import_needs_no_grant`.

/// A component that wants the network.
const WANTS_NETWORK: &str = r#"
(component
  (import "wasi:sockets/tcp@0.2.6" (instance (export "connect" (func))))
)
"#;

/// A component that wants several things at once.
const WANTS_SEVERAL: &str = r#"
(component
  (import "wasi:io/streams@0.2.6" (instance (export "read" (func))))
  (import "wasi:filesystem/types@0.2.6" (instance (export "open" (func))))
  (import "wasi:clocks/wall-clock@0.2.6" (instance (export "now" (func))))
  (import "wasi:random/random@0.2.6" (instance (export "get" (func))))
)
"#;

/// A component that wants the application's own interface.
const WANTS_HOST_INTERFACE: &str = r#"
(component
  (import "watoots:example/log@0.1.0" (instance (export "emit" (func))))
)
"#;

/// A component importing an interface that holds only type definitions.
const WANTS_TYPES_ONLY: &str = r#"
(component
  (import "watoots:example/types@0.1.0" (instance))
)
"#;

fn host_with(manifest_toml: &str) -> Host {
    Host::builder()
        .manifest(Manifest::parse(manifest_toml).unwrap())
        .build()
        .unwrap()
}

#[test]
fn a_self_contained_component_loads_and_runs_under_an_empty_manifest() {
    let host = host_with("");
    let mut plugin = host
        .load_binary("answer", SELF_CONTAINED.as_bytes())
        .unwrap();

    assert_eq!(plugin.name(), "answer");
    assert!(plugin.grants().is_satisfied());

    let results = plugin.call("answer", &[]).unwrap();
    assert_eq!(results, vec![watoots::Val::S32(42)]);
}

#[test]
fn an_ungranted_import_fails_the_load_rather_than_the_call() {
    // This is the whole thesis: you learn the plugin wants the network when you
    // install it, not the first time it reaches for a socket.
    let host = host_with("");
    let err = host
        .load_binary("net", WANTS_NETWORK.as_bytes())
        .unwrap_err();

    assert_eq!(err.kind(), ErrorKind::PermissionDenied);
    assert!(
        err.message().contains("wasi:sockets/tcp"),
        "{}",
        err.message()
    );
}

#[test]
fn a_denial_names_the_manifest_key_that_would_grant_it() {
    let host = host_with("");
    let err = host
        .load_binary("net", WANTS_NETWORK.as_bytes())
        .unwrap_err();
    assert!(
        err.message().contains("permissions.net"),
        "denial should say how to fix it, got: {}",
        err.message()
    );
}

#[test]
fn inspect_reports_without_instantiating() {
    let host = host_with("");
    let report = host.inspect(WANTS_SEVERAL.as_bytes()).unwrap();

    assert!(!report.is_satisfied());
    let denied: Vec<&str> = report.denied().map(|d| d.import.as_str()).collect();
    assert_eq!(
        denied,
        [
            "wasi:filesystem/types@0.2.6",
            "wasi:clocks/wall-clock@0.2.6",
            "wasi:random/random@0.2.6",
        ],
        "{}",
        report.describe()
    );

    // wasi:io is plumbing and conveys nothing on its own.
    assert_eq!(report.decisions[0].requirement, Requirement::Ambient);
    assert!(report.decisions[0].granted);
}

#[test]
fn granting_each_capability_satisfies_the_check() {
    let dir = std::env::temp_dir();
    let host = host_with(&format!(
        r#"
        [permissions]
        fs.read = ["{}"]
        clocks  = "wall"
        random  = true
        "#,
        dir.display()
    ));

    let report = host.inspect(WANTS_SEVERAL.as_bytes()).unwrap();
    assert!(report.is_satisfied(), "{}", report.describe());
}

#[test]
fn a_host_interface_is_granted_only_when_the_application_declares_it() {
    let undeclared = host_with("");
    let report = undeclared.inspect(WANTS_HOST_INTERFACE.as_bytes()).unwrap();
    assert!(!report.is_satisfied(), "{}", report.describe());

    let declared = Host::builder()
        .provide_interface("watoots:example/log")
        .build()
        .unwrap();
    let report = declared.inspect(WANTS_HOST_INTERFACE.as_bytes()).unwrap();
    assert!(report.is_satisfied(), "{}", report.describe());
    assert_eq!(report.decisions[0].requirement, Requirement::HostProvided);
}

#[test]
fn calling_a_missing_export_is_not_found() {
    let host = host_with("");
    let mut plugin = host
        .load_binary("answer", SELF_CONTAINED.as_bytes())
        .unwrap();
    let err = plugin.call("nope", &[]).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);
}

#[test]
fn garbage_bytes_fail_to_load() {
    let host = host_with("");
    let err = host.load_binary("junk", b"not a component").unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Load);
}

#[test]
fn a_missing_file_reports_not_found() {
    let host = host_with("");
    let err = host.load("does/not/exist.wasm").unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);
}

#[test]
fn a_net_allowlist_is_refused_until_it_is_enforced() {
    // Better to fail closed and say so than to hand over the whole network
    // because the manifest named one host.
    let err = Host::builder()
        .manifest(Manifest::parse("[permissions]\nnet = [\"example.com\"]\n").unwrap())
        .build()
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Manifest);
    assert!(
        err.message().contains("not enforced yet"),
        "{}",
        err.message()
    );
    assert!(err.message().contains("net = []"), "{}", err.message());
}

#[test]
fn an_empty_net_list_admits_the_interface_with_nothing_reachable() {
    // A CPython or JavaScript runtime links the socket interfaces whether or
    // not the plugin opens one. Denying the import would refuse those guests
    // outright; granting the import while wasmtime-wasi refuses every
    // connection is what actually matches the situation.
    let denied = host_with("");
    assert!(denied.load_binary("net", WANTS_NETWORK.as_bytes()).is_err());

    let granted = host_with("[permissions]\nnet = []\n");
    let report = granted.inspect(WANTS_NETWORK.as_bytes()).unwrap();
    assert!(report.is_satisfied(), "{}", report.describe());
}

#[test]
fn a_type_only_import_needs_no_grant() {
    // WIT packages routinely pull in a sibling interface just for its types --
    // `log` using `severity` from `types` puts the whole types interface in the
    // import list. There is nothing callable there, so denying it would refuse
    // every real plugin for a capability it cannot exercise.
    let host = host_with("");
    let report = host.inspect(WANTS_TYPES_ONLY.as_bytes()).unwrap();

    assert!(report.is_satisfied(), "{}", report.describe());
    assert_eq!(report.decisions[0].requirement, Requirement::TypesOnly);
}
