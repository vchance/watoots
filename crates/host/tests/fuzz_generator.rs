//! The generator, and the property everything else is built on: a value the
//! generator produces survives a round trip through WAVE.
//!
//! Properties 1 and 4 of [ADR-0008]. If this one does not hold there is no
//! point running the trace properties, because every value in a trace is WAVE
//! text.
//!
//! The types come from a component, because `wasmtime::component::Type` has no
//! public constructor — you get one from a compiled component or not at all.
//! The zoo below is that component: nothing but an imported instance declaring
//! functions whose parameters cover the type space. It is never instantiated,
//! so it needs no core module, no memory and no realloc.
//!
//! [ADR-0008]: ../../../docs/adr/0008-fuzzing.md

use std::sync::OnceLock;

use proptest::prelude::*;
use watoots::fuzz::Generator;
use watoots::{Host, Manifest, Type, Val, from_wave, to_wave};

/// Every type WAVE can spell, declared as parameters of three imported
/// functions.
///
/// Index-based rather than named, for the reason `crates/host/tests/logging.rs`
/// gives at length: an instance type used as an import may only reference types
/// it also exports, and the text format cannot bind a name to the *exported*
/// alias. Two further rules the numbering has to respect, learned the hard way:
/// exporting a type consumes an index of its own, and a compound type written
/// inline in a function signature does too — which is why every compound type
/// here is a numbered declarator and the signatures name only indices.
const ZOO: &str = r#"
(component
  (type (;0;)
    (instance
      ;; Nominal types: a record, an enum, flags, variants. Each is defined and
      ;; then exported, and it is the exported alias the signatures use.
      (type (;0;) (record (field "line" u32) (field "col" u32) (field "message" string)))
      (export (;1;) "diag" (type (eq 0)))
      (type (;2;) (enum "error" "warning" "hint"))
      (export (;3;) "sev" (type (eq 2)))
      (type (;4;) (flags "read" "write" "exec"))
      (export (;5;) "perm" (type (eq 4)))
      (type (;6;) (variant (case "empty") (case "count" u32) (case "label" string) (case "diag" 1)))
      (export (;7;) "shape" (type (eq 6)))
      ;; One-of-each edges: a single flag, a single unit case.
      (type (;8;) (flags "solo"))
      (export (;9;) "one-flag" (type (eq 8)))
      (type (;10;) (variant (case "only")))
      (export (;11;) "one-case" (type (eq 10)))

      ;; Structural types, including all four shapes of `result` and enough
      ;; nesting to exercise the generator's depth budget.
      (type (;12;) (result))
      (type (;13;) (result (error string)))
      (type (;14;) (result u32))
      (type (;15;) (result u32 (error string)))
      (type (;16;) (option string))
      (type (;17;) (option 16))
      (type (;18;) (list u8))
      (type (;19;) (tuple bool char string f32))
      (type (;20;) (list 1))
      (type (;21;) (option 20))
      (type (;22;) (list 21))
      (type (;23;) (list s16))
      (type (;24;) (list 23))
      (type (;25;) (list 24))
      (type (;26;) (tuple 7 5 3))

      (type (;27;) (func (param "a" bool) (param "b" s8) (param "c" u8) (param "d" s16)
                         (param "e" u16) (param "f" s32) (param "g" u32) (param "h" s64)
                         (param "i" u64) (param "j" f32) (param "k" f64) (param "l" char)
                         (param "m" string)))
      (type (;28;) (func (param "d" 1) (param "s" 3) (param "p" 5) (param "v" 7)
                         (param "o" 9) (param "n" 11) (param "t" 26)))
      (type (;29;) (func (param "a" 12) (param "b" 13) (param "c" 14) (param "d" 15)
                         (param "e" 17) (param "f" 18) (param "g" 19) (param "h" 22)
                         (param "i" 25)))
      (export (;0;) "prims" (func (type 27)))
      (export (;1;) "nominal" (func (type 28)))
      (export (;2;) "structural" (func (type 29)))
    )
  )
  (import "watoots:fuzz/zoo@0.1.0" (instance (type 0)))
)
"#;

/// A world that passes a resource handle across the boundary — the one thing
/// the generator has to refuse rather than approximate.
const WITH_A_RESOURCE: &str = r#"
(component
  (type (;0;)
    (instance
      (export (;0;) "conn" (type (sub resource)))
      (type (;1;) (own 0))
      (type (;2;) (func (param "c" 1)))
      (export (;0;) "close" (func (type 2)))
    )
  )
  (import "watoots:fuzz/res@0.1.0" (instance (type 0)))
)
"#;

/// Every parameter type in the zoo, flattened.
///
/// Compiled once: a `Type` owns its type table by `Arc`, so it outlives the
/// engine that produced it and there is no reason to pay for the compile per
/// case.
fn zoo_types() -> &'static [Type] {
    static TYPES: OnceLock<Vec<Type>> = OnceLock::new();
    TYPES.get_or_init(|| import_param_types(ZOO))
}

fn import_param_types(wat: &str) -> Vec<Type> {
    use wasmtime::component::types::ComponentItem;

    let engine = wasmtime::Engine::default();
    let component =
        wasmtime::component::Component::new(&engine, wat.as_bytes()).expect("the zoo compiles");

    let mut types = Vec::new();
    for (_, item) in component.component_type().imports(&engine) {
        let ComponentItem::ComponentInstance(instance) = &item.ty else {
            continue;
        };
        for (_, nested) in instance.exports(&engine) {
            if let ComponentItem::ComponentFunc(func) = &nested.ty {
                types.extend(func.params().map(|(_, ty)| ty));
            }
        }
    }
    assert!(!types.is_empty(), "the zoo declares no parameters");
    types
}

/// The case budget, overridable for a longer local run.
///
/// `cargo test --workspace` has to stay something a contributor runs without
/// thinking about it, so the defaults are small and a real campaign is
/// `watoots fuzz`.
fn config(default_cases: u32) -> ProptestConfig {
    let cases = std::env::var("WATOOTS_PROPTEST_CASES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default_cases);
    ProptestConfig {
        cases,
        ..ProptestConfig::default()
    }
}

/// The bytes a generator draws from. proptest shrinks these toward fewer and
/// smaller, which shrinks the generated value toward empty lists and zeroes.
fn entropy() -> impl Strategy<Value = Vec<u8>> {
    proptest::collection::vec(any::<u8>(), 0..2048)
}

/// `Val`'s own equality, with one documented exception for `flags`.
///
/// **This exception is a finding, not a convenience.** WAVE renders a flag set
/// in the order the `Val` holds it — the world's declared order, which is what
/// lifting from a guest produces — but `wasm-wave` 0.254 parses one through a
/// `BTreeMap` used to reject duplicate labels and hands back
/// `into_values()`, so a parsed set comes out sorted alphabetically
/// (`wasm-wave-0.254.0/src/parser.rs`, `finish_flags`). `Val: PartialEq`
/// compares flags positionally, so `from_wave(ty, &to_wave(v))? == v` is false
/// for any set of two or more flags whose declaration order is not
/// alphabetical: `{write, exec}` parses back as `["exec", "write"]`.
///
/// It is left alone rather than corrected here. It is upstream of watoots — a
/// wrapper is all `crates/host/src/wave.rs` is meant to be, per ADR-0004 — and
/// it changes nothing that record/replay depends on: a trace stores the
/// rendered *text* and compares text, and lowering a flag set back into a guest
/// maps labels to bits without consulting their order. What it does mean is
/// that a `Val` should not be compared positionally after a WAVE round trip,
/// which is exactly what this helper says. See
/// `wave_sorts_a_flag_set_on_the_way_back_in` for the pinned behaviour.
fn values_agree(left: &Val, right: &Val) -> bool {
    match (left, right) {
        (Val::Flags(left), Val::Flags(right)) => {
            let mut left = left.clone();
            let mut right = right.clone();
            left.sort();
            right.sort();
            left == right
        }
        (Val::List(left), Val::List(right))
        | (Val::Tuple(left), Val::Tuple(right))
        | (Val::FixedLengthList(left), Val::FixedLengthList(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|(left, right)| values_agree(left, right))
        }
        (Val::Record(left), Val::Record(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|(left, right)| left.0 == right.0 && values_agree(&left.1, &right.1))
        }
        (Val::Variant(left_name, left_payload), Val::Variant(right_name, right_payload)) => {
            left_name == right_name && payloads_agree(left_payload, right_payload)
        }
        (Val::Option(left), Val::Option(right)) => payloads_agree(left, right),
        (Val::Result(Ok(left)), Val::Result(Ok(right)))
        | (Val::Result(Err(left)), Val::Result(Err(right))) => payloads_agree(left, right),
        _ => left == right,
    }
}

fn payloads_agree(left: &Option<Box<Val>>, right: &Option<Box<Val>>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => values_agree(left, right),
        _ => false,
    }
}

proptest! {
    #![proptest_config(config(256))]

    /// Property 1: `Val` → WAVE → `Val` is the identity, for every type WAVE
    /// can spell.
    #[test]
    fn a_value_survives_a_round_trip_through_wave(bytes in entropy()) {
        let mut generator = Generator::from_bytes(bytes);
        for ty in zoo_types() {
            let value = generator.value(ty).expect("the zoo declares no resources");
            let text = match to_wave(&value) {
                Ok(text) => text,
                Err(err) => {
                    return Err(TestCaseError::fail(format!(
                        "{value:?} of type {ty:?} has no WAVE rendering: {}",
                        err.message()
                    )));
                }
            };
            let parsed = from_wave(ty, &text).map_err(|err| {
                TestCaseError::fail(format!(
                    "{text:?} does not parse back as {ty:?}: {}",
                    err.message()
                ))
            })?;
            prop_assert!(
                values_agree(&parsed, &value),
                "round trip through {text:?} changed the value:\n  before: {value:?}\n  after:  {parsed:?}"
            );
        }
    }

    /// Property 4, on the host's dynamic surface: whatever text a caller hands
    /// `from_wave`, an `Err` is a fine answer and a panic is not.
    #[test]
    fn parsing_arbitrary_text_as_a_value_never_panics(
        text in ".{0,64}",
        which in 0usize..64,
    ) {
        let types = zoo_types();
        let _ = from_wave(&types[which % types.len()], &text);
    }

    /// Property 4, on the manifest parser. It is the least interesting code in
    /// the repository to fuzz — ADR-0008 says so — but it is also the one an
    /// untrusted-looking file reaches first.
    #[test]
    fn parsing_arbitrary_text_as_a_manifest_never_panics(text in ".{0,256}") {
        let _ = Manifest::parse(&text);
    }

    /// Property 4, on the loader. Random bytes are almost never a component,
    /// which is exactly ADR-0008's argument for type-driven generation — but
    /// "almost never valid" still has to mean `Err`, not a crash.
    #[test]
    fn inspecting_arbitrary_bytes_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..256)) {
        let host = Host::builder().build().unwrap();
        let _ = host.inspect(&bytes);
        let _ = host.load_binary("junk", &bytes);
    }
}

#[test]
fn the_zoo_covers_every_type_wave_can_spell() {
    // A generator that silently stopped covering a type would make property 1
    // pass by not testing anything. Name the shapes rather than counting them.
    let mut seen = std::collections::BTreeSet::new();
    fn walk(ty: &Type, seen: &mut std::collections::BTreeSet<&'static str>) {
        seen.insert(match ty {
            Type::Bool => "bool",
            Type::S8 => "s8",
            Type::U8 => "u8",
            Type::S16 => "s16",
            Type::U16 => "u16",
            Type::S32 => "s32",
            Type::U32 => "u32",
            Type::S64 => "s64",
            Type::U64 => "u64",
            Type::Float32 => "f32",
            Type::Float64 => "f64",
            Type::Char => "char",
            Type::String => "string",
            Type::List(_) => "list",
            Type::Record(_) => "record",
            Type::Tuple(_) => "tuple",
            Type::Variant(_) => "variant",
            Type::Enum(_) => "enum",
            Type::Option(_) => "option",
            Type::Result(_) => "result",
            Type::Flags(_) => "flags",
            other => panic!("the zoo should not contain {other:?}"),
        });
        match ty {
            Type::List(list) => walk(&list.ty(), seen),
            Type::Option(option) => walk(&option.ty(), seen),
            Type::Tuple(tuple) => tuple.types().for_each(|ty| walk(&ty, seen)),
            Type::Record(record) => record.fields().for_each(|field| walk(&field.ty, seen)),
            Type::Variant(variant) => variant
                .cases()
                .filter_map(|case| case.ty)
                .for_each(|ty| walk(&ty, seen)),
            Type::Result(result) => [result.ok(), result.err()]
                .into_iter()
                .flatten()
                .for_each(|ty| walk(&ty, seen)),
            _ => {}
        }
    }

    for ty in zoo_types() {
        walk(ty, &mut seen);
    }

    for expected in [
        "bool", "s8", "u8", "s16", "u16", "s32", "u32", "s64", "u64", "f32", "f64", "char",
        "string", "list", "record", "tuple", "variant", "enum", "option", "result", "flags",
    ] {
        assert!(
            seen.contains(expected),
            "the zoo has no {expected}: {seen:?}"
        );
    }
}

#[test]
fn every_zoo_type_is_declared_supported() {
    watoots::fuzz::supported(zoo_types()).expect("the zoo is entirely fuzzable");
}

#[test]
fn a_resource_handle_is_refused_by_name() {
    // Not "failed to parse WAVE" three layers down: the refusal has to say
    // resource, because that is the thing the user has to change. ADR-0004.
    let types = import_param_types(WITH_A_RESOURCE);
    assert_eq!(types.len(), 1, "{types:?}");

    let err = watoots::fuzz::supported(&types).unwrap_err();
    assert_eq!(err.kind(), watoots::ErrorKind::InvalidArgument);
    assert!(
        err.message().contains("resource handle"),
        "{}",
        err.message()
    );
    assert!(err.message().contains("0004"), "{}", err.message());

    let err = Generator::from_seed(1).value(&types[0]).unwrap_err();
    assert!(
        err.message().contains("resource handle"),
        "{}",
        err.message()
    );
}

#[test]
fn wave_sorts_a_flag_set_on_the_way_back_in() {
    // Found by `a_value_survives_a_round_trip_through_wave`, and pinned here
    // rather than papered over. `wasm-wave` 0.254 parses a flag set through a
    // BTreeMap (it uses the map to reject duplicate labels) and returns
    // `into_values()`, so the labels come back alphabetical rather than in the
    // order they were written or declared. `Val` compares flags positionally,
    // so the round trip is not the identity.
    //
    // Left alone deliberately: it is upstream, and nothing watoots does
    // depends on the order — a trace compares rendered text, and lowering a
    // flag set into a guest maps labels to bits. If a future wasm-wave stops
    // sorting, this test fails and `values_agree` can lose its exception.
    let ty = zoo_types()
        .iter()
        .find(|ty| {
            matches!(ty, Type::Flags(flags) if flags.names().collect::<Vec<_>>() == ["read", "write", "exec"])
        })
        .expect("the zoo declares flags read/write/exec");

    let declared = Val::Flags(vec!["write".to_string(), "exec".to_string()]);
    let text = to_wave(&declared).unwrap();
    assert_eq!(text, "{write, exec}");
    assert_eq!(
        from_wave(ty, &text).unwrap(),
        Val::Flags(vec!["exec".to_string(), "write".to_string()]),
        "wasm-wave no longer sorts flag labels; values_agree can drop its exception"
    );
}

#[test]
fn the_same_seed_generates_the_same_arguments() {
    // A crash report carries a seed. If this drifts the report is worthless.
    let types = zoo_types();
    let first = Generator::from_seed(42).values(types).unwrap();
    let second = Generator::from_seed(42).values(types).unwrap();
    assert_eq!(first, second);
    assert_ne!(first, Generator::from_seed(43).values(types).unwrap());
}
