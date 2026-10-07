//! Typed values across the C boundary.
//!
//! [`wt_plugin_call`](crate::wt_plugin_call) takes and returns WAVE text,
//! which is the right shape for a linter and the wrong one for a codec: a
//! `list<u8>` of a million pixels is four million characters of `12,` to
//! render on one side and parse on the other. This module is the other path.
//! A `wt_val_t` is a `wasmtime::component::Val` behind an opaque pointer, built
//! from C with one constructor per WIT kind and read back with one accessor per
//! kind, and [`wt_plugin_call_vals`] carries them across with no text in
//! between.
//!
//! The two paths are the same call underneath -- the same limits, the same
//! trace events, the same audit -- so a recording made through one replays
//! through the other. The trace format stays WAVE text, because a file is read
//! by people and this struct is read by a program.
//!
//! # Ownership
//!
//! A constructor returns an owned value; free it with [`wt_val_delete`]. A
//! constructor that takes child values **takes ownership of them** -- every
//! item of a list, every field of a record, the payload of a variant -- whether
//! or not it succeeds, so a `NULL` from a constructor never leaks what was
//! handed to it, and a value is never freed twice because it was put inside
//! another one. A value is immutable once built; `wt_val_clone` is the way to
//! reuse one.
//!
//! An accessor that returns a `const wt_val_t*` or a `const char*` is handing
//! out a **borrow**: it lives as long as the value it was read from and must
//! not be freed. Strings borrowed this way are **not NUL-terminated** -- a WIT
//! string may contain a NUL, so a length comes with every one.
//!
//! A value is not synchronised. Read it from one thread at a time.

use std::ffi::{CStr, c_char};
use std::ptr;

use watoots::{Error, Val};

use crate::{borrow_str, guard, into_c_string, wt_error_t, wt_plugin_t, wt_status};

/// A typed WIT value. Opaque: build one with a `wt_val_*` constructor and read
/// it with a `wt_val_as_*` accessor.
//
// `repr(transparent)` is what lets an accessor hand out a child of a list or a
// record as a `const wt_val_t*` without copying it: the child is a `Val` inside
// the parent, and a `&Val` is a `&wt_val_t` by layout. cbindgen is told to
// treat the type as opaque, see cbindgen.toml.
#[repr(transparent)]
pub struct wt_val_t {
    inner: Val,
}

impl wt_val_t {
    fn view(val: &Val) -> *const wt_val_t {
        // SAFETY of the later dereference: `wt_val_t` is `repr(transparent)`
        // over `Val`, so the pointer is valid for as long as `val` is.
        ptr::from_ref(val).cast()
    }

    fn own(val: Val) -> *mut wt_val_t {
        Box::into_raw(Box::new(wt_val_t { inner: val }))
    }
}

/// Which WIT kind a value is. `WT_VAL_OTHER` is a value this API cannot
/// express -- a resource handle, a stream, a future -- which can be passed
/// through unchanged but not built or read.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum wt_val_kind {
    WT_VAL_BOOL = 0,
    WT_VAL_S8 = 1,
    WT_VAL_U8 = 2,
    WT_VAL_S16 = 3,
    WT_VAL_U16 = 4,
    WT_VAL_S32 = 5,
    WT_VAL_U32 = 6,
    WT_VAL_S64 = 7,
    WT_VAL_U64 = 8,
    WT_VAL_F32 = 9,
    WT_VAL_F64 = 10,
    WT_VAL_CHAR = 11,
    WT_VAL_STRING = 12,
    WT_VAL_LIST = 13,
    WT_VAL_RECORD = 14,
    WT_VAL_TUPLE = 15,
    WT_VAL_VARIANT = 16,
    WT_VAL_ENUM = 17,
    WT_VAL_OPTION = 18,
    WT_VAL_RESULT = 19,
    WT_VAL_FLAGS = 20,
    WT_VAL_OTHER = 21,
}

impl From<&Val> for wt_val_kind {
    fn from(val: &Val) -> Self {
        match val {
            Val::Bool(_) => wt_val_kind::WT_VAL_BOOL,
            Val::S8(_) => wt_val_kind::WT_VAL_S8,
            Val::U8(_) => wt_val_kind::WT_VAL_U8,
            Val::S16(_) => wt_val_kind::WT_VAL_S16,
            Val::U16(_) => wt_val_kind::WT_VAL_U16,
            Val::S32(_) => wt_val_kind::WT_VAL_S32,
            Val::U32(_) => wt_val_kind::WT_VAL_U32,
            Val::S64(_) => wt_val_kind::WT_VAL_S64,
            Val::U64(_) => wt_val_kind::WT_VAL_U64,
            Val::Float32(_) => wt_val_kind::WT_VAL_F32,
            Val::Float64(_) => wt_val_kind::WT_VAL_F64,
            Val::Char(_) => wt_val_kind::WT_VAL_CHAR,
            Val::String(_) => wt_val_kind::WT_VAL_STRING,
            // A fixed-length list reads exactly like a list; the length is the
            // type's business, not the value's.
            Val::List(_) | Val::FixedLengthList(_) => wt_val_kind::WT_VAL_LIST,
            Val::Record(_) => wt_val_kind::WT_VAL_RECORD,
            Val::Tuple(_) => wt_val_kind::WT_VAL_TUPLE,
            Val::Variant(..) => wt_val_kind::WT_VAL_VARIANT,
            Val::Enum(_) => wt_val_kind::WT_VAL_ENUM,
            Val::Option(_) => wt_val_kind::WT_VAL_OPTION,
            Val::Result(_) => wt_val_kind::WT_VAL_RESULT,
            Val::Flags(_) => wt_val_kind::WT_VAL_FLAGS,
            _ => wt_val_kind::WT_VAL_OTHER,
        }
    }
}

/// The WIT spelling of a kind, e.g. `"list"`. Never NULL; static storage.
#[unsafe(no_mangle)]
pub extern "C" fn wt_val_kind_name(kind: wt_val_kind) -> *const c_char {
    let name: &CStr = match kind {
        wt_val_kind::WT_VAL_BOOL => c"bool",
        wt_val_kind::WT_VAL_S8 => c"s8",
        wt_val_kind::WT_VAL_U8 => c"u8",
        wt_val_kind::WT_VAL_S16 => c"s16",
        wt_val_kind::WT_VAL_U16 => c"u16",
        wt_val_kind::WT_VAL_S32 => c"s32",
        wt_val_kind::WT_VAL_U32 => c"u32",
        wt_val_kind::WT_VAL_S64 => c"s64",
        wt_val_kind::WT_VAL_U64 => c"u64",
        wt_val_kind::WT_VAL_F32 => c"f32",
        wt_val_kind::WT_VAL_F64 => c"f64",
        wt_val_kind::WT_VAL_CHAR => c"char",
        wt_val_kind::WT_VAL_STRING => c"string",
        wt_val_kind::WT_VAL_LIST => c"list",
        wt_val_kind::WT_VAL_RECORD => c"record",
        wt_val_kind::WT_VAL_TUPLE => c"tuple",
        wt_val_kind::WT_VAL_VARIANT => c"variant",
        wt_val_kind::WT_VAL_ENUM => c"enum",
        wt_val_kind::WT_VAL_OPTION => c"option",
        wt_val_kind::WT_VAL_RESULT => c"result",
        wt_val_kind::WT_VAL_FLAGS => c"flags",
        wt_val_kind::WT_VAL_OTHER => c"other",
    };
    name.as_ptr()
}

// ---------------------------------------------------------------------------
// Building
// ---------------------------------------------------------------------------

/// Take ownership of `len` child values handed to a constructor.
///
/// Every pointer is consumed, so the caller's handles are dead after this
/// whether or not the constructor goes on to succeed. A NULL among them makes
/// the whole construction fail -- a NULL is what a failed constructor returned,
/// and silently dropping it would build a value with a hole in it.
unsafe fn take_children(items: *const *mut wt_val_t, len: usize) -> Option<Vec<Val>> {
    if len == 0 {
        return Some(Vec::new());
    }
    if items.is_null() {
        return None;
    }
    let mut taken = Vec::with_capacity(len);
    let mut complete = true;
    for index in 0..len {
        let raw = unsafe { *items.add(index) };
        if raw.is_null() {
            complete = false;
            continue;
        }
        // SAFETY: the contract of every constructor is that children are owned
        // `wt_val_t` pointers, handed over here.
        let child = unsafe { Box::from_raw(raw) };
        taken.push(child.inner);
    }
    complete.then_some(taken)
}

/// Take ownership of one optional child.
unsafe fn take_child(raw: *mut wt_val_t) -> Option<Val> {
    if raw.is_null() {
        None
    } else {
        // SAFETY: as in `take_children`.
        Some(unsafe { Box::from_raw(raw) }.inner)
    }
}

unsafe fn take_names(names: *const *const c_char, len: usize) -> Option<Vec<String>> {
    if len == 0 {
        return Some(Vec::new());
    }
    if names.is_null() {
        return None;
    }
    let mut out = Vec::with_capacity(len);
    for index in 0..len {
        let raw = unsafe { *names.add(index) };
        if raw.is_null() {
            return None;
        }
        out.push(unsafe { CStr::from_ptr(raw) }.to_str().ok()?.to_owned());
    }
    Some(out)
}

/// A `bool`.
#[unsafe(no_mangle)]
pub extern "C" fn wt_val_bool(value: bool) -> *mut wt_val_t {
    wt_val_t::own(Val::Bool(value))
}

/// An `s8`.
#[unsafe(no_mangle)]
pub extern "C" fn wt_val_s8(value: i8) -> *mut wt_val_t {
    wt_val_t::own(Val::S8(value))
}

/// A `u8`.
#[unsafe(no_mangle)]
pub extern "C" fn wt_val_u8(value: u8) -> *mut wt_val_t {
    wt_val_t::own(Val::U8(value))
}

/// An `s16`.
#[unsafe(no_mangle)]
pub extern "C" fn wt_val_s16(value: i16) -> *mut wt_val_t {
    wt_val_t::own(Val::S16(value))
}

/// A `u16`.
#[unsafe(no_mangle)]
pub extern "C" fn wt_val_u16(value: u16) -> *mut wt_val_t {
    wt_val_t::own(Val::U16(value))
}

/// An `s32`.
#[unsafe(no_mangle)]
pub extern "C" fn wt_val_s32(value: i32) -> *mut wt_val_t {
    wt_val_t::own(Val::S32(value))
}

/// A `u32`.
#[unsafe(no_mangle)]
pub extern "C" fn wt_val_u32(value: u32) -> *mut wt_val_t {
    wt_val_t::own(Val::U32(value))
}

/// An `s64`.
#[unsafe(no_mangle)]
pub extern "C" fn wt_val_s64(value: i64) -> *mut wt_val_t {
    wt_val_t::own(Val::S64(value))
}

/// A `u64`.
#[unsafe(no_mangle)]
pub extern "C" fn wt_val_u64(value: u64) -> *mut wt_val_t {
    wt_val_t::own(Val::U64(value))
}

/// An `f32`.
#[unsafe(no_mangle)]
pub extern "C" fn wt_val_f32(value: f32) -> *mut wt_val_t {
    wt_val_t::own(Val::Float32(value))
}

/// An `f64`.
#[unsafe(no_mangle)]
pub extern "C" fn wt_val_f64(value: f64) -> *mut wt_val_t {
    wt_val_t::own(Val::Float64(value))
}

/// A `char`, from a Unicode scalar value. NULL if `codepoint` is a surrogate
/// or out of range -- WIT `char` is a scalar value, not a code unit.
#[unsafe(no_mangle)]
pub extern "C" fn wt_val_char(codepoint: u32) -> *mut wt_val_t {
    match char::from_u32(codepoint) {
        Some(c) => wt_val_t::own(Val::Char(c)),
        None => ptr::null_mut(),
    }
}

/// A `string`, from `len` bytes of UTF-8 that need not be NUL-terminated.
/// NULL if the bytes are not valid UTF-8.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wt_val_string(utf8: *const c_char, len: usize) -> *mut wt_val_t {
    if len == 0 {
        return wt_val_t::own(Val::String(String::new()));
    }
    if utf8.is_null() {
        return ptr::null_mut();
    }
    let bytes = unsafe { std::slice::from_raw_parts(utf8.cast::<u8>(), len) };
    match std::str::from_utf8(bytes) {
        Ok(text) => wt_val_t::own(Val::String(text.to_owned())),
        Err(_) => ptr::null_mut(),
    }
}

/// A `string` from a NUL-terminated C string. NULL if not valid UTF-8.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wt_val_cstring(text: *const c_char) -> *mut wt_val_t {
    if text.is_null() {
        return ptr::null_mut();
    }
    match unsafe { CStr::from_ptr(text) }.to_str() {
        Ok(text) => wt_val_t::own(Val::String(text.to_owned())),
        Err(_) => ptr::null_mut(),
    }
}

/// A `list<u8>` from a byte buffer. This is the constructor the WAVE path has
/// no answer to: the bytes are copied once and never rendered as text.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wt_val_bytes(data: *const u8, len: usize) -> *mut wt_val_t {
    if len == 0 {
        return wt_val_t::own(Val::List(Vec::new()));
    }
    if data.is_null() {
        return ptr::null_mut();
    }
    let bytes = unsafe { std::slice::from_raw_parts(data, len) };
    wt_val_t::own(Val::List(bytes.iter().copied().map(Val::U8).collect()))
}

/// A `list<T>` of `len` items, which it takes ownership of. NULL if any item
/// is NULL; the others are still freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wt_val_list(items: *const *mut wt_val_t, len: usize) -> *mut wt_val_t {
    match unsafe { take_children(items, len) } {
        Some(items) => wt_val_t::own(Val::List(items)),
        None => ptr::null_mut(),
    }
}

/// A `tuple<...>` of `len` items, which it takes ownership of. NULL if any
/// item is NULL; the others are still freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wt_val_tuple(items: *const *mut wt_val_t, len: usize) -> *mut wt_val_t {
    match unsafe { take_children(items, len) } {
        Some(items) => wt_val_t::own(Val::Tuple(items)),
        None => ptr::null_mut(),
    }
}

/// A `record` of `len` fields, `names[i]` holding `values[i]`. Takes ownership
/// of the values. NULL if a name or a value is NULL, or a name is not UTF-8;
/// the values are still freed. Field order is the WIT declaration order.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wt_val_record(
    names: *const *const c_char,
    values: *const *mut wt_val_t,
    len: usize,
) -> *mut wt_val_t {
    // Values first, so they are consumed even when the names are bad.
    let values = unsafe { take_children(values, len) };
    let names = unsafe { take_names(names, len) };
    match (names, values) {
        (Some(names), Some(values)) => {
            wt_val_t::own(Val::Record(names.into_iter().zip(values).collect()))
        }
        _ => ptr::null_mut(),
    }
}

/// A `variant` case, with its payload or NULL for a case that carries none.
/// Takes ownership of the payload.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wt_val_variant(
    case_name: *const c_char,
    payload: *mut wt_val_t,
) -> *mut wt_val_t {
    let payload = unsafe { take_child(payload) };
    if case_name.is_null() {
        return ptr::null_mut();
    }
    match unsafe { CStr::from_ptr(case_name) }.to_str() {
        Ok(name) => wt_val_t::own(Val::Variant(name.to_owned(), payload.map(Box::new))),
        Err(_) => ptr::null_mut(),
    }
}

/// An `enum` case.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wt_val_enum(case_name: *const c_char) -> *mut wt_val_t {
    if case_name.is_null() {
        return ptr::null_mut();
    }
    match unsafe { CStr::from_ptr(case_name) }.to_str() {
        Ok(name) => wt_val_t::own(Val::Enum(name.to_owned())),
        Err(_) => ptr::null_mut(),
    }
}

/// An `option<T>`: `some(value)`, or `none` when `value` is NULL. Takes
/// ownership of the value.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wt_val_option(value: *mut wt_val_t) -> *mut wt_val_t {
    let value = unsafe { take_child(value) };
    wt_val_t::own(Val::Option(value.map(Box::new)))
}

/// A `result`'s `ok` case, with its payload or NULL for a result whose ok
/// type is absent. Takes ownership of the payload.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wt_val_ok(payload: *mut wt_val_t) -> *mut wt_val_t {
    let payload = unsafe { take_child(payload) };
    wt_val_t::own(Val::Result(Ok(payload.map(Box::new))))
}

/// A `result`'s `err` case, with its payload or NULL for a result whose err
/// type is absent. Takes ownership of the payload.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wt_val_err(payload: *mut wt_val_t) -> *mut wt_val_t {
    let payload = unsafe { take_child(payload) };
    wt_val_t::own(Val::Result(Err(payload.map(Box::new))))
}

/// A `flags` value with `len` flags set, by name. NULL if a name is NULL or
/// not UTF-8.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wt_val_flags(names: *const *const c_char, len: usize) -> *mut wt_val_t {
    match unsafe { take_names(names, len) } {
        Some(names) => wt_val_t::own(Val::Flags(names)),
        None => ptr::null_mut(),
    }
}

/// A deep copy. NULL only if `value` is NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wt_val_clone(value: *const wt_val_t) -> *mut wt_val_t {
    if value.is_null() {
        return ptr::null_mut();
    }
    wt_val_t::own(unsafe { &*value }.inner.clone())
}

/// Free a value and everything inside it. NULL is a no-op.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wt_val_delete(value: *mut wt_val_t) {
    if !value.is_null() {
        // SAFETY: created by `wt_val_t::own`, and the caller promises not to
        // use it again.
        drop(unsafe { Box::from_raw(value) });
    }
}

/// Render a value as WAVE text, to free with `wt_string_delete`. NULL if the
/// value is NULL or has no WAVE spelling (a resource, a stream, a future).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wt_val_to_wave(value: *const wt_val_t) -> *mut c_char {
    if value.is_null() {
        return ptr::null_mut();
    }
    match watoots::to_wave(&unsafe { &*value }.inner) {
        Ok(text) => into_c_string(&text).unwrap_or(ptr::null_mut()),
        Err(_) => ptr::null_mut(),
    }
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

/// Which kind `value` is. `WT_VAL_OTHER` for NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wt_val_kind_of(value: *const wt_val_t) -> wt_val_kind {
    if value.is_null() {
        return wt_val_kind::WT_VAL_OTHER;
    }
    wt_val_kind::from(&unsafe { &*value }.inner)
}

unsafe fn inner<'a>(value: *const wt_val_t) -> Option<&'a Val> {
    if value.is_null() {
        None
    } else {
        Some(&unsafe { &*value }.inner)
    }
}

/// Read a `bool`. False if `value` is not one; `*out` is then untouched.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wt_val_as_bool(value: *const wt_val_t, out: *mut bool) -> bool {
    match unsafe { inner(value) } {
        Some(Val::Bool(b)) if !out.is_null() => {
            unsafe { *out = *b };
            true
        }
        _ => false,
    }
}

/// Read any integer that fits in an `int64_t`: every signed kind, and every
/// unsigned one up to `u64` values below 2^63. False otherwise.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wt_val_as_s64(value: *const wt_val_t, out: *mut i64) -> bool {
    let number = match unsafe { inner(value) } {
        Some(Val::S8(n)) => i64::from(*n),
        Some(Val::S16(n)) => i64::from(*n),
        Some(Val::S32(n)) => i64::from(*n),
        Some(Val::S64(n)) => *n,
        Some(Val::U8(n)) => i64::from(*n),
        Some(Val::U16(n)) => i64::from(*n),
        Some(Val::U32(n)) => i64::from(*n),
        Some(Val::U64(n)) => match i64::try_from(*n) {
            Ok(n) => n,
            Err(_) => return false,
        },
        _ => return false,
    };
    if out.is_null() {
        return false;
    }
    unsafe { *out = number };
    true
}

/// Read any non-negative integer: every unsigned kind, and every signed one
/// holding a value of zero or more. False otherwise.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wt_val_as_u64(value: *const wt_val_t, out: *mut u64) -> bool {
    let number = match unsafe { inner(value) } {
        Some(Val::U8(n)) => u64::from(*n),
        Some(Val::U16(n)) => u64::from(*n),
        Some(Val::U32(n)) => u64::from(*n),
        Some(Val::U64(n)) => *n,
        Some(Val::S8(n)) => match u64::try_from(*n) {
            Ok(n) => n,
            Err(_) => return false,
        },
        Some(Val::S16(n)) => match u64::try_from(*n) {
            Ok(n) => n,
            Err(_) => return false,
        },
        Some(Val::S32(n)) => match u64::try_from(*n) {
            Ok(n) => n,
            Err(_) => return false,
        },
        Some(Val::S64(n)) => match u64::try_from(*n) {
            Ok(n) => n,
            Err(_) => return false,
        },
        _ => return false,
    };
    if out.is_null() {
        return false;
    }
    unsafe { *out = number };
    true
}

/// Read an `f32` or `f64` as a double. False otherwise.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wt_val_as_f64(value: *const wt_val_t, out: *mut f64) -> bool {
    let number = match unsafe { inner(value) } {
        Some(Val::Float32(f)) => f64::from(*f),
        Some(Val::Float64(f)) => *f,
        _ => return false,
    };
    if out.is_null() {
        return false;
    }
    unsafe { *out = number };
    true
}

/// Read a `char` as its Unicode scalar value. False otherwise.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wt_val_as_char(value: *const wt_val_t, out: *mut u32) -> bool {
    match unsafe { inner(value) } {
        Some(Val::Char(c)) if !out.is_null() => {
            unsafe { *out = u32::from(*c) };
            true
        }
        _ => false,
    }
}

/// Borrow a `string` as `len` bytes of UTF-8, **not NUL-terminated**. False if
/// `value` is not a string. The pointer lives as long as `value`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wt_val_as_string(
    value: *const wt_val_t,
    data: *mut *const c_char,
    len: *mut usize,
) -> bool {
    match unsafe { inner(value) } {
        Some(Val::String(text)) if !data.is_null() && !len.is_null() => {
            unsafe {
                *data = text.as_ptr().cast();
                *len = text.len();
            }
            true
        }
        _ => false,
    }
}

/// Copy a `list<u8>` out into `out`, which holds `cap` bytes.
///
/// False if `value` is not a list whose every item is a `u8`. Otherwise `*len`
/// is set to the list's length and the first `min(len, cap)` bytes are
/// written, so a NULL `out` with a zero `cap` sizes the buffer, and a caller
/// that finds `*len > cap` afterwards knows the copy was cut short. An empty
/// list is a `list<u8>`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wt_val_as_bytes(
    value: *const wt_val_t,
    out: *mut u8,
    cap: usize,
    len: *mut usize,
) -> bool {
    let items = match unsafe { inner(value) } {
        Some(Val::List(items) | Val::FixedLengthList(items)) => items,
        _ => return false,
    };
    if len.is_null() || !items.iter().all(|item| matches!(item, Val::U8(_))) {
        return false;
    }
    unsafe { *len = items.len() };
    if out.is_null() || cap == 0 {
        return true;
    }
    let dest = unsafe { std::slice::from_raw_parts_mut(out, cap.min(items.len())) };
    for (slot, item) in dest.iter_mut().zip(items) {
        if let Val::U8(byte) = item {
            *slot = *byte;
        }
    }
    true
}

/// How many items a `list` or `tuple` has, fields a `record` has, or flags a
/// `flags` value has set. Zero for anything else.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wt_val_len(value: *const wt_val_t) -> usize {
    match unsafe { inner(value) } {
        Some(Val::List(items) | Val::FixedLengthList(items) | Val::Tuple(items)) => items.len(),
        Some(Val::Record(fields)) => fields.len(),
        Some(Val::Flags(names)) => names.len(),
        _ => 0,
    }
}

/// Borrow item `index` of a `list` or `tuple`. NULL if out of range or not
/// one of those. Lives as long as `value`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wt_val_item(value: *const wt_val_t, index: usize) -> *const wt_val_t {
    match unsafe { inner(value) } {
        Some(Val::List(items) | Val::FixedLengthList(items) | Val::Tuple(items)) => {
            items.get(index).map_or(ptr::null(), wt_val_t::view)
        }
        _ => ptr::null(),
    }
}

/// Borrow a `record` field by name. NULL if there is no such field or `value`
/// is not a record. Lives as long as `value`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wt_val_field(
    value: *const wt_val_t,
    name: *const c_char,
) -> *const wt_val_t {
    let (Some(Val::Record(fields)), false) = (unsafe { inner(value) }, name.is_null()) else {
        return ptr::null();
    };
    let Ok(name) = unsafe { CStr::from_ptr(name) }.to_str() else {
        return ptr::null();
    };
    fields
        .iter()
        .find(|(field, _)| field == name)
        .map_or(ptr::null(), |(_, val)| wt_val_t::view(val))
}

/// Borrow `record` field number `index`, in declaration order. NULL if out of
/// range or not a record.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wt_val_field_at(value: *const wt_val_t, index: usize) -> *const wt_val_t {
    match unsafe { inner(value) } {
        Some(Val::Record(fields)) => fields
            .get(index)
            .map_or(ptr::null(), |(_, val)| wt_val_t::view(val)),
        _ => ptr::null(),
    }
}

/// Borrow the name of `record` field number `index`, **not NUL-terminated**,
/// writing its length to `*len`. NULL if out of range or not a record.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wt_val_field_name(
    value: *const wt_val_t,
    index: usize,
    len: *mut usize,
) -> *const c_char {
    match unsafe { inner(value) } {
        Some(Val::Record(fields)) if !len.is_null() => match fields.get(index) {
            Some((name, _)) => {
                unsafe { *len = name.len() };
                name.as_ptr().cast()
            }
            None => ptr::null(),
        },
        _ => ptr::null(),
    }
}

/// Borrow the case name of a `variant` or `enum`, **not NUL-terminated**.
/// False if `value` is neither.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wt_val_case(
    value: *const wt_val_t,
    name: *mut *const c_char,
    len: *mut usize,
) -> bool {
    let case = match unsafe { inner(value) } {
        Some(Val::Variant(case, _) | Val::Enum(case)) => case,
        _ => return false,
    };
    if name.is_null() || len.is_null() {
        return false;
    }
    unsafe {
        *name = case.as_ptr().cast();
        *len = case.len();
    }
    true
}

/// Borrow the payload of a `variant` case, an `option`'s `some`, or a
/// `result`'s `ok` or `err`. NULL when there is none: a payload-less case,
/// `none`, or a result side without a type. Use `wt_val_is_ok` to tell `none`
/// from `some(unit)` and `ok` from `err`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wt_val_payload(value: *const wt_val_t) -> *const wt_val_t {
    let payload = match unsafe { inner(value) } {
        Some(Val::Variant(_, payload) | Val::Option(payload)) => payload.as_deref(),
        Some(Val::Result(Ok(payload) | Err(payload))) => payload.as_deref(),
        _ => None,
    };
    payload.map_or(ptr::null(), wt_val_t::view)
}

/// Whether a `result` is `ok`, or an `option` is `some`. False if `value` is
/// neither; `*out` is then untouched.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wt_val_is_ok(value: *const wt_val_t, out: *mut bool) -> bool {
    let ok = match unsafe { inner(value) } {
        Some(Val::Result(result)) => result.is_ok(),
        Some(Val::Option(option)) => option.is_some(),
        _ => return false,
    };
    if out.is_null() {
        return false;
    }
    unsafe { *out = ok };
    true
}

/// Borrow the name of set flag number `index` of a `flags` value, **not
/// NUL-terminated**. NULL if out of range or not a flags value.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wt_val_flag_at(
    value: *const wt_val_t,
    index: usize,
    len: *mut usize,
) -> *const c_char {
    match unsafe { inner(value) } {
        Some(Val::Flags(names)) if !len.is_null() => match names.get(index) {
            Some(name) => {
                unsafe { *len = name.len() };
                name.as_ptr().cast()
            }
            None => ptr::null(),
        },
        _ => ptr::null(),
    }
}

// ---------------------------------------------------------------------------
// Calling
// ---------------------------------------------------------------------------

/// Call an exported function with typed arguments.
///
/// The typed twin of `wt_plugin_call`: the same limits, the same trace and
/// audit events, no text in between. `args` are borrowed, not consumed. On
/// success `*result_out` is either NULL, when the function returns nothing, or
/// an owned value to free with [`wt_val_delete`].
///
/// An argument of the wrong kind for the parameter is reported as
/// `WT_ERR_INVALID_ARGUMENT` by the component's own type check, before any
/// guest code runs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wt_plugin_call_vals(
    plugin: *mut wt_plugin_t,
    export: *const c_char,
    args: *const *const wt_val_t,
    args_len: usize,
    result_out: *mut *mut wt_val_t,
    error_out: *mut *mut wt_error_t,
) -> wt_status {
    guard(error_out, || {
        if plugin.is_null() || result_out.is_null() {
            return Err(Error::invalid_argument(
                "plugin and result_out must not be NULL",
            ));
        }
        if args.is_null() && args_len != 0 {
            return Err(Error::invalid_argument("args must not be NULL"));
        }
        let export = unsafe { borrow_str(export, "export") }?;

        let mut values = Vec::with_capacity(args_len);
        for index in 0..args_len {
            let raw = unsafe { *args.add(index) };
            let Some(val) = (unsafe { inner(raw) }) else {
                return Err(Error::invalid_argument(format!(
                    "argument {index} must not be NULL"
                )));
            };
            values.push(val.clone());
        }

        let plugin = unsafe { &mut (*plugin).inner };
        let mut results = plugin.call(export, &values)?;

        unsafe { *result_out = ptr::null_mut() };
        match results.len() {
            0 => Ok(()),
            1 => {
                unsafe { *result_out = wt_val_t::own(results.remove(0)) };
                Ok(())
            }
            // As in `wt_plugin_call`: WIT 0.2 functions return at most one
            // value, so more is the world having moved on.
            more => Err(Error::internal(format!(
                "{export} returned {more} values; the C API expects at most one"
            ))),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    fn text(raw: *const c_char, len: usize) -> String {
        let bytes = unsafe { std::slice::from_raw_parts(raw.cast::<u8>(), len) };
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[test]
    fn a_record_of_bytes_round_trips_through_wave() {
        let names = [c"width".as_ptr(), c"pixels".as_ptr()];
        let pixels = [1u8, 2, 3, 255];
        let values = [wt_val_u32(2), unsafe { wt_val_bytes(pixels.as_ptr(), 4) }];
        let record = unsafe { wt_val_record(names.as_ptr(), values.as_ptr(), 2) };
        assert!(!record.is_null());

        let wave = unsafe { wt_val_to_wave(record) };
        let rendered = unsafe { CStr::from_ptr(wave) }.to_str().unwrap().to_owned();
        assert_eq!(rendered, "{width: 2, pixels: [1, 2, 3, 255]}");
        unsafe { crate::wt_string_delete(wave) };

        assert_eq!(
            unsafe { wt_val_kind_of(record) },
            wt_val_kind::WT_VAL_RECORD
        );
        assert_eq!(unsafe { wt_val_len(record) }, 2);

        let mut len = 0;
        let name = unsafe { wt_val_field_name(record, 1, &mut len) };
        assert_eq!(text(name, len), "pixels");

        let field = unsafe { wt_val_field(record, c"pixels".as_ptr()) };
        assert!(!field.is_null());
        let mut buf = [0u8; 2];
        let mut got = 0;
        assert!(unsafe { wt_val_as_bytes(field, buf.as_mut_ptr(), 2, &mut got) });
        assert_eq!(got, 4, "the full length is reported even when cut short");
        assert_eq!(buf, [1, 2]);
        let mut full = [0u8; 4];
        assert!(unsafe { wt_val_as_bytes(field, full.as_mut_ptr(), 4, &mut got) });
        assert_eq!(full, pixels);

        let mut width = 0u64;
        assert!(unsafe { wt_val_as_u64(wt_val_field(record, c"width".as_ptr()), &mut width) });
        assert_eq!(width, 2);
        assert!(unsafe { wt_val_field(record, c"height".as_ptr()) }.is_null());

        unsafe { wt_val_delete(record) };
    }

    #[test]
    fn a_constructor_consumes_its_children_even_when_it_fails() {
        // A NULL item fails the list; the live one next to it must be freed
        // rather than leaked, and the test is that this does not double-free
        // or crash under the allocator's own checks.
        let items = [wt_val_u8(1), ptr::null_mut()];
        assert!(unsafe { wt_val_list(items.as_ptr(), 2) }.is_null());

        let values = [wt_val_bool(true)];
        let bad_name = [ptr::null::<c_char>()];
        assert!(unsafe { wt_val_record(bad_name.as_ptr(), values.as_ptr(), 1) }.is_null());
    }

    #[test]
    fn accessors_refuse_the_wrong_kind_and_leave_out_untouched() {
        let s = unsafe { wt_val_cstring(c"hi".as_ptr()) };
        let mut b = true;
        assert!(!unsafe { wt_val_as_bool(s, &mut b) });
        assert!(b);
        let mut n = 7i64;
        assert!(!unsafe { wt_val_as_s64(s, &mut n) });
        assert_eq!(n, 7);
        assert!(unsafe { wt_val_item(s, 0) }.is_null());
        assert_eq!(unsafe { wt_val_len(s) }, 0);

        let mut data = ptr::null();
        let mut len = 0;
        assert!(unsafe { wt_val_as_string(s, &mut data, &mut len) });
        assert_eq!(text(data, len), "hi");
        unsafe { wt_val_delete(s) };

        // A list that is not all bytes is not a list<u8>.
        let mixed = [wt_val_u8(1), wt_val_u16(2)];
        let list = unsafe { wt_val_list(mixed.as_ptr(), 2) };
        let mut got = 0;
        assert!(!unsafe { wt_val_as_bytes(list, ptr::null_mut(), 0, &mut got) });
        unsafe { wt_val_delete(list) };

        // An empty list is.
        let empty = unsafe { wt_val_bytes(ptr::null(), 0) };
        assert!(unsafe { wt_val_as_bytes(empty, ptr::null_mut(), 0, &mut got) });
        assert_eq!(got, 0);
        unsafe { wt_val_delete(empty) };
    }

    #[test]
    fn integers_convert_across_width_but_not_across_sign() {
        let big = wt_val_u64(u64::MAX);
        let mut s = 0i64;
        assert!(!unsafe { wt_val_as_s64(big, &mut s) });
        let mut u = 0u64;
        assert!(unsafe { wt_val_as_u64(big, &mut u) });
        assert_eq!(u, u64::MAX);
        unsafe { wt_val_delete(big) };

        let neg = wt_val_s8(-1);
        assert!(!unsafe { wt_val_as_u64(neg, &mut u) });
        assert!(unsafe { wt_val_as_s64(neg, &mut s) });
        assert_eq!(s, -1);
        unsafe { wt_val_delete(neg) };
    }

    #[test]
    fn results_options_and_variants_expose_case_and_payload() {
        let err = unsafe { wt_val_err(wt_val_variant(c"truncated".as_ptr(), wt_val_u64(9))) };
        let mut ok = true;
        assert!(unsafe { wt_val_is_ok(err, &mut ok) });
        assert!(!ok);
        let failure = unsafe { wt_val_payload(err) };
        let mut name = ptr::null();
        let mut len = 0;
        assert!(unsafe { wt_val_case(failure, &mut name, &mut len) });
        assert_eq!(text(name, len), "truncated");
        let mut missing = 0u64;
        assert!(unsafe { wt_val_as_u64(wt_val_payload(failure), &mut missing) });
        assert_eq!(missing, 9);
        unsafe { wt_val_delete(err) };

        let none = unsafe { wt_val_option(ptr::null_mut()) };
        assert!(unsafe { wt_val_is_ok(none, &mut ok) });
        assert!(!ok);
        assert!(unsafe { wt_val_payload(none) }.is_null());
        unsafe { wt_val_delete(none) };

        let bare = unsafe { wt_val_variant(c"not-this-format".as_ptr(), ptr::null_mut()) };
        assert!(unsafe { wt_val_payload(bare) }.is_null());
        let wave = unsafe { wt_val_to_wave(bare) };
        assert_eq!(
            unsafe { CStr::from_ptr(wave) }.to_str().unwrap(),
            "not-this-format"
        );
        unsafe { crate::wt_string_delete(wave) };
        unsafe { wt_val_delete(bare) };

        let flags = [c"read".as_ptr(), c"exec".as_ptr()];
        let set = unsafe { wt_val_flags(flags.as_ptr(), 2) };
        assert_eq!(unsafe { wt_val_len(set) }, 2);
        assert_eq!(
            text(unsafe { wt_val_flag_at(set, 1, &mut len) }, len),
            "exec"
        );
        unsafe { wt_val_delete(set) };

        assert!(wt_val_char(0xD800).is_null(), "a surrogate is not a char");
        let bad = CString::new([0xFFu8, 0xFE].to_vec()).unwrap();
        assert!(unsafe { wt_val_cstring(bad.as_ptr()) }.is_null());
    }

    #[test]
    fn every_kind_has_a_name() {
        for kind in [
            wt_val_kind::WT_VAL_BOOL,
            wt_val_kind::WT_VAL_LIST,
            wt_val_kind::WT_VAL_OTHER,
        ] {
            assert!(!wt_val_kind_name(kind).is_null());
        }
        assert_eq!(
            unsafe { CStr::from_ptr(wt_val_kind_name(wt_val_kind::WT_VAL_RECORD)) },
            c"record"
        );
    }
}
