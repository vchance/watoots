//! `Host::check_targets`: does a component implement the world we intend to
//! call? The complement to the import-intersection check, which only ever
//! looked at imports.

use std::io::Write as _;

use watoots::{ErrorKind, Host};

/// A component exporting `answer: func() -> s32`, and nothing else.
const ANSWERS: &str = r#"
(component
  (core module $m
    (func (export "answer") (result i32) i32.const 42))
  (core instance $i (instantiate $m))
  (func $answer (result s32) (canon lift (core func $i "answer")))
  (export "answer" (func $answer))
)
"#;

fn component(wat: &str) -> Vec<u8> {
    wat::parse_str(wat).expect("assembling the test component")
}

fn wit_file(contents: &str) -> tempfile::NamedTempFile {
    let mut file = tempfile::Builder::new()
        .suffix(".wit")
        .tempfile()
        .expect("creating a WIT file");
    file.write_all(contents.as_bytes()).expect("writing WIT");
    file.flush().expect("flushing WIT");
    file
}

#[test]
fn a_component_that_implements_the_world_passes() {
    let wit = wit_file(
        r#"
        package test:answers@0.1.0;
        world answering {
          export answer: func() -> s32;
        }
        "#,
    );
    let host = Host::builder().build().unwrap();
    host.check_targets(&component(ANSWERS), wit.path(), Some("answering"))
        .expect("the component exports exactly this world");
}

#[test]
fn a_missing_export_is_a_load_error_not_a_permission_error() {
    // The same component, against a world that also wants `question`.
    let wit = wit_file(
        r#"
        package test:answers@0.1.0;
        world demanding {
          export answer: func() -> s32;
          export question: func() -> string;
        }
        "#,
    );
    let host = Host::builder().build().unwrap();
    let err = host
        .check_targets(&component(ANSWERS), wit.path(), Some("demanding"))
        .expect_err("the component does not export `question`");
    assert_eq!(err.kind(), ErrorKind::Load);
    assert!(err.message().contains("demanding"), "{}", err.message());
}

#[test]
fn a_world_that_does_not_exist_is_not_found() {
    let wit = wit_file(
        r#"
        package test:answers@0.1.0;
        world answering {
          export answer: func() -> s32;
        }
        "#,
    );
    let host = Host::builder().build().unwrap();
    let err = host
        .check_targets(&component(ANSWERS), wit.path(), Some("nope"))
        .expect_err("no such world");
    assert_eq!(err.kind(), ErrorKind::NotFound);
}

#[test]
fn wit_that_does_not_parse_is_an_invalid_argument() {
    let wit = wit_file("this is not WIT");
    let host = Host::builder().build().unwrap();
    let err = host
        .check_targets(&component(ANSWERS), wit.path(), None)
        .expect_err("unparseable WIT");
    assert_eq!(err.kind(), ErrorKind::InvalidArgument);
}
