//! The dynamic call path: what a caller gets back when the arguments are the
//! caller's problem rather than the plugin's.

use watoots::{ErrorKind, Host, Manifest, Val};

/// Takes a `u32`, returns it doubled.
const DOUBLER: &str = r#"
(component
  (core module $m
    (func (export "double") (param i32) (result i32)
      (i32.mul (local.get 0) (i32.const 2))))
  (core instance $i (instantiate $m))
  (func $double (param "n" u32) (result u32) (canon lift (core func $i "double")))
  (export "double" (func $double))
)
"#;

fn host() -> Host {
    Host::builder()
        .manifest(Manifest::parse("").unwrap())
        .build()
        .unwrap()
}

#[test]
fn a_typed_call_carries_the_value_both_ways() {
    let host = host();
    let mut plugin = host.load_binary("doubler", DOUBLER.as_bytes()).unwrap();
    let out = plugin.call("double", &[Val::U32(21)]).unwrap();
    assert_eq!(out, vec![Val::U32(42)]);
}

/// A host's own mistake is reported as one, and -- the part that matters --
/// costs the host an error rather than the plugin its instance: wasmtime
/// refuses to re-enter a component after a failed call, so a mismatch that
/// reached it would have turned every later call into "cannot enter component
/// instance". The check runs first, so nothing was entered.
#[test]
fn a_wrong_kind_of_argument_is_the_callers_error() {
    let host = host();
    let mut plugin = host.load_binary("doubler", DOUBLER.as_bytes()).unwrap();

    let err = plugin
        .call("double", &[Val::String("21".into())])
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidArgument, "{err}");
    assert!(
        err.message()
            .contains("argument 0 (n): type mismatch: expected u32, found string"),
        "{err}"
    );

    let err = plugin.call("double", &[]).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidArgument, "{err}");
    assert!(
        err.message().contains("expected 1 argument(s), got 0"),
        "{err}"
    );

    // And the plugin is still fine: the mistake was ours, nothing trapped.
    assert_eq!(
        plugin.call("double", &[Val::U32(4)]).unwrap(),
        vec![Val::U32(8)]
    );
}
