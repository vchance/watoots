//! Does this `Val` have the shape this `Type` declares?
//!
//! Wasmtime answers the same question while lowering the arguments of a
//! dynamic call -- but it answers it *inside* the call, after marking the
//! instance as entered, and a mismatch found there leaves the instance refusing
//! every later call with "cannot enter component instance". That is the right
//! outcome for a trap: the guest is in an unknown state. It is the wrong
//! outcome for a host that passed a `string` where the world says `u32`, which
//! is the host's bug and should cost the host an error, not the plugin its
//! life.
//!
//! So [`Plugin::call`](crate::Plugin::call) asks here first. The walk is the
//! same structural check wasmtime makes, done before anything is entered, and
//! it reports `ErrorKind::InvalidArgument` with a path to the offending value
//! rather than a trap with a one-line cause. The WAVE path never needed this:
//! parsing text against the parameter type cannot produce a mismatched `Val`.
//! The typed C path, and any Rust caller building `Val`s by hand, can.

use wasmtime::component::{Type, Val};

use crate::{Error, Result};

/// Check every argument against its parameter, naming the first mismatch.
pub(crate) fn check_args(
    plugin: &str,
    export: &str,
    params: &[(&str, Type)],
    args: &[Val],
) -> Result<()> {
    if params.len() != args.len() {
        return Err(Error::invalid_argument(format!(
            "{plugin}: {export}: expected {} argument(s), got {}",
            params.len(),
            args.len()
        )));
    }
    for (index, ((name, ty), val)) in params.iter().zip(args).enumerate() {
        if let Err(Mismatch {
            path,
            expected,
            found,
        }) = conforms(val, ty)
        {
            let at = if path.is_empty() {
                String::new()
            } else {
                format!(" at {path}")
            };
            return Err(Error::invalid_argument(format!(
                "{plugin}: {export}: argument {index} ({name}){at}: type mismatch: \
                     expected {expected}, found {found}"
            )));
        }
    }
    Ok(())
}

/// Where and how a value departed from its type. `path` is empty at the top
/// level and otherwise reads like `[3].pixels`.
struct Mismatch {
    path: String,
    expected: String,
    found: &'static str,
}

impl Mismatch {
    fn here(ty: &Type, val: &Val) -> Self {
        Mismatch {
            path: String::new(),
            expected: describe_type(ty),
            found: describe_val(val),
        }
    }

    fn shaped(expected: String, found: &'static str) -> Self {
        Mismatch {
            path: String::new(),
            expected,
            found,
        }
    }

    /// Prefix the path with the step that led into the child.
    fn under(mut self, step: &str) -> Self {
        self.path.insert_str(0, step);
        self
    }
}

fn conforms(val: &Val, ty: &Type) -> std::result::Result<(), Mismatch> {
    match (ty, val) {
        (Type::Bool, Val::Bool(_))
        | (Type::S8, Val::S8(_))
        | (Type::U8, Val::U8(_))
        | (Type::S16, Val::S16(_))
        | (Type::U16, Val::U16(_))
        | (Type::S32, Val::S32(_))
        | (Type::U32, Val::U32(_))
        | (Type::S64, Val::S64(_))
        | (Type::U64, Val::U64(_))
        | (Type::Float32, Val::Float32(_))
        | (Type::Float64, Val::Float64(_))
        | (Type::Char, Val::Char(_))
        | (Type::String, Val::String(_))
        | (Type::Own(_) | Type::Borrow(_), Val::Resource(_))
        | (Type::Future(_), Val::Future(_))
        | (Type::Stream(_), Val::Stream(_))
        | (Type::ErrorContext, Val::ErrorContext(_)) => {}

        (Type::List(list), Val::List(items)) => {
            let item_ty = list.ty();
            for (index, item) in items.iter().enumerate() {
                conforms(item, &item_ty).map_err(|m| m.under(&format!("[{index}]")))?;
            }
        }
        (Type::FixedLengthList(list), Val::FixedLengthList(items) | Val::List(items)) => {
            if items.len() != list.len() as usize {
                return Err(Mismatch::shaped(
                    format!("a list of {} item(s)", list.len()),
                    "a different count",
                ));
            }
            let item_ty = list.ty();
            for (index, item) in items.iter().enumerate() {
                conforms(item, &item_ty).map_err(|m| m.under(&format!("[{index}]")))?;
            }
        }
        (Type::Map(map), Val::Map(pairs)) => {
            let (key_ty, value_ty) = (map.key(), map.value());
            for (index, (key, value)) in pairs.iter().enumerate() {
                conforms(key, &key_ty).map_err(|m| m.under(&format!("[{index}].key")))?;
                conforms(value, &value_ty).map_err(|m| m.under(&format!("[{index}].value")))?;
            }
        }
        (Type::Record(record), Val::Record(fields)) => {
            if record.fields().len() != fields.len() {
                return Err(Mismatch::shaped(
                    format!("a record of {} field(s)", record.fields().len()),
                    "a different count",
                ));
            }
            for (field, (name, value)) in record.fields().zip(fields) {
                if field.name != name {
                    return Err(Mismatch::shaped(
                        format!("field `{}`", field.name),
                        "a field by another name",
                    ));
                }
                conforms(value, &field.ty).map_err(|m| m.under(&format!(".{name}")))?;
            }
        }
        (Type::Tuple(tuple), Val::Tuple(items)) => {
            if tuple.types().len() != items.len() {
                return Err(Mismatch::shaped(
                    format!("a tuple of {} item(s)", tuple.types().len()),
                    "a different count",
                ));
            }
            for (index, (item_ty, item)) in tuple.types().zip(items).enumerate() {
                conforms(item, &item_ty).map_err(|m| m.under(&format!(".{index}")))?;
            }
        }
        (Type::Enum(enum_), Val::Enum(case)) => {
            if !enum_.names().any(|known| known == case) {
                return Err(Mismatch::shaped(
                    format!("a case of {}", describe_type(ty)),
                    "an unknown case",
                ));
            }
        }
        (Type::Variant(variant), Val::Variant(case, payload)) => {
            let Some(found) = variant.cases().find(|c| c.name == case) else {
                return Err(Mismatch::shaped(
                    format!("a case of {}", describe_type(ty)),
                    "an unknown case",
                ));
            };
            conforms_payload(
                found.ty.as_ref(),
                payload.as_deref(),
                &format!("case `{case}`"),
            )
            .map_err(|m| m.under(&format!(".{case}")))?;
        }
        (Type::Option(option), Val::Option(payload)) => {
            if let Some(payload) = payload {
                conforms(payload, &option.ty()).map_err(|m| m.under(".some"))?;
            }
        }
        (Type::Result(result), Val::Result(value)) => {
            let (side, declared, payload) = match value {
                Ok(payload) => ("ok", result.ok(), payload),
                Err(payload) => ("err", result.err(), payload),
            };
            conforms_payload(declared.as_ref(), payload.as_deref(), &format!("`{side}`"))
                .map_err(|m| m.under(&format!(".{side}")))?;
        }
        (Type::Flags(flags), Val::Flags(set)) => {
            for name in set {
                if !flags.names().any(|known| known == name) {
                    return Err(Mismatch::shaped(
                        format!("a flag of {}", describe_type(ty)),
                        "an unknown flag",
                    ));
                }
            }
        }

        _ => return Err(Mismatch::here(ty, val)),
    }
    Ok(())
}

/// A variant case or a result side either carries a payload or does not, and
/// the value has to agree with the type about which.
///
/// `what` names the case or side for the message. A mismatch inside the payload
/// is reported at the payload's own path; the caller prefixes the step.
fn conforms_payload(
    declared: Option<&Type>,
    payload: Option<&Val>,
    what: &str,
) -> std::result::Result<(), Mismatch> {
    match (declared, payload) {
        (Some(payload_ty), Some(payload)) => conforms(payload, payload_ty),
        (None, None) => Ok(()),
        (Some(_), None) => Err(Mismatch::shaped(format!("a payload for {what}"), "none")),
        (None, Some(_)) => Err(Mismatch::shaped(format!("no payload for {what}"), "one")),
    }
}

/// A type as WIT would spell it, one level deep.
fn describe_type(ty: &Type) -> String {
    match ty {
        Type::Bool => "bool".into(),
        Type::S8 => "s8".into(),
        Type::U8 => "u8".into(),
        Type::S16 => "s16".into(),
        Type::U16 => "u16".into(),
        Type::S32 => "s32".into(),
        Type::U32 => "u32".into(),
        Type::S64 => "s64".into(),
        Type::U64 => "u64".into(),
        Type::Float32 => "f32".into(),
        Type::Float64 => "f64".into(),
        Type::Char => "char".into(),
        Type::String => "string".into(),
        Type::List(list) => format!("list<{}>", describe_type(&list.ty())),
        Type::FixedLengthList(list) => {
            format!("list<{}, {}>", describe_type(&list.ty()), list.len())
        }
        Type::Map(map) => format!(
            "map<{}, {}>",
            describe_type(&map.key()),
            describe_type(&map.value())
        ),
        Type::Record(record) => {
            let names: Vec<&str> = record.fields().map(|f| f.name).collect();
            format!("record {{{}}}", names.join(", "))
        }
        Type::Tuple(tuple) => {
            let types: Vec<String> = tuple.types().map(|t| describe_type(&t)).collect();
            format!("tuple<{}>", types.join(", "))
        }
        Type::Variant(variant) => {
            let names: Vec<&str> = variant.cases().map(|c| c.name).collect();
            format!("variant {{{}}}", names.join(", "))
        }
        Type::Enum(e) => {
            let names: Vec<&str> = e.names().collect();
            format!("enum {{{}}}", names.join(", "))
        }
        Type::Option(option) => format!("option<{}>", describe_type(&option.ty())),
        Type::Result(result) => {
            let side = |t: Option<Type>| t.map_or("_".to_owned(), |t| describe_type(&t));
            format!("result<{}, {}>", side(result.ok()), side(result.err()))
        }
        Type::Flags(flags) => {
            let names: Vec<&str> = flags.names().collect();
            format!("flags {{{}}}", names.join(", "))
        }
        Type::Own(_) => "own<resource>".into(),
        Type::Borrow(_) => "borrow<resource>".into(),
        Type::Future(_) => "future".into(),
        Type::Stream(_) => "stream".into(),
        Type::ErrorContext => "error-context".into(),
    }
}

/// A value's kind, by the same vocabulary.
fn describe_val(val: &Val) -> &'static str {
    match val {
        Val::Bool(_) => "bool",
        Val::S8(_) => "s8",
        Val::U8(_) => "u8",
        Val::S16(_) => "s16",
        Val::U16(_) => "u16",
        Val::S32(_) => "s32",
        Val::U32(_) => "u32",
        Val::S64(_) => "s64",
        Val::U64(_) => "u64",
        Val::Float32(_) => "f32",
        Val::Float64(_) => "f64",
        Val::Char(_) => "char",
        Val::String(_) => "string",
        Val::List(_) | Val::FixedLengthList(_) => "list",
        Val::Map(_) => "map",
        Val::Record(_) => "record",
        Val::Tuple(_) => "tuple",
        Val::Variant(..) => "variant",
        Val::Enum(_) => "enum",
        Val::Option(_) => "option",
        Val::Result(_) => "result",
        Val::Flags(_) => "flags",
        Val::Resource(_) => "resource",
        Val::Future(_) => "future",
        Val::Stream(_) => "stream",
        Val::ErrorContext(_) => "error-context",
    }
}
