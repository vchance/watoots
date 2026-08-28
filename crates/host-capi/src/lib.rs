//! The C API for watoots.
//!
//! Every public Rust API in `watoots` was designed with this surface in mind:
//! opaque handles, no generics across the boundary, and errors as a code plus a
//! message string. Values cross as WAVE text (`"notes.md"`, `{line: 2}`), which
//! is why the host library has a dynamic call path at all — a C caller has
//! strings, and the component's own type says how to read them.
//!
//! # Ownership
//!
//! Every `wt_*_new` has a matching `wt_*_delete`, and every `char*` written to
//! an out-parameter is freed with [`wt_string_delete`]. Pointers returned by
//! [`wt_plugin_name`] and [`wt_error_message`] are borrowed and live only as
//! long as the object they came from.
//!
//! Functions that can fail return a [`wt_status`] and take a `wt_error_t**`.
//! Passing `NULL` for that out-parameter is allowed and discards the message.
//!
//! # Panics
//!
//! Unwinding across the FFI boundary is undefined behaviour, so every entry
//! point catches panics and turns them into `WT_ERR_INTERNAL`.

#![allow(non_camel_case_types)]
#![allow(clippy::missing_safety_doc)]

use std::ffi::{CStr, CString, c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;

use watoots::{Error, ErrorKind, Host, HostBuilder, HostCall, Manifest, Plugin, Val};

/// Status codes. Zero is success.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum wt_status {
    WT_OK = 0,
    WT_ERR_INVALID_ARGUMENT = 1,
    WT_ERR_NOT_FOUND = 2,
    WT_ERR_MANIFEST = 3,
    WT_ERR_PERMISSION_DENIED = 4,
    WT_ERR_LOAD = 5,
    WT_ERR_TRAP = 6,
    WT_ERR_LIMIT_EXCEEDED = 7,
    WT_ERR_INTERNAL = 8,
}

impl From<ErrorKind> for wt_status {
    fn from(kind: ErrorKind) -> Self {
        match kind {
            ErrorKind::InvalidArgument => Self::WT_ERR_INVALID_ARGUMENT,
            ErrorKind::NotFound => Self::WT_ERR_NOT_FOUND,
            ErrorKind::Manifest => Self::WT_ERR_MANIFEST,
            ErrorKind::PermissionDenied => Self::WT_ERR_PERMISSION_DENIED,
            ErrorKind::Load => Self::WT_ERR_LOAD,
            ErrorKind::Trap => Self::WT_ERR_TRAP,
            ErrorKind::LimitExceeded => Self::WT_ERR_LIMIT_EXCEEDED,
            _ => Self::WT_ERR_INTERNAL,
        }
    }
}

/// An error: a status code and a message.
pub struct wt_error_t {
    code: wt_status,
    message: CString,
}

/// Builder for a host. Consumed by [`wt_host_builder_build`].
pub struct wt_host_builder_t {
    inner: Option<HostBuilder>,
}

/// A configured host.
pub struct wt_host_t {
    inner: Host,
}

/// A loaded plugin.
pub struct wt_plugin_t {
    inner: Plugin,
    /// Kept so [`wt_plugin_name`] can hand out a stable pointer.
    name: CString,
}

/// A function the application serves to plugins.
///
/// `args` holds `args_len` WAVE strings. Write at most one WAVE string to
/// `result_out` — allocate it with [`wt_string_new`] — or leave it untouched if
/// the function returns nothing. On failure, return a non-zero status and
/// optionally set `*error_out` with [`wt_error_new`].
///
/// The callback and `userdata` must be safe to use from any thread: watoots
/// does not serialise calls into it.
pub type wt_host_func_t = Option<
    unsafe extern "C" fn(
        userdata: *mut c_void,
        args: *const *const c_char,
        args_len: usize,
        result_out: *mut *mut c_char,
        error_out: *mut *mut wt_error_t,
    ) -> wt_status,
>;

// ---------------------------------------------------------------------------
// Plumbing
// ---------------------------------------------------------------------------

/// Run `body`, turning a panic into `WT_ERR_INTERNAL` rather than unwinding
/// into C, which would be undefined behaviour.
fn guard(error_out: *mut *mut wt_error_t, body: impl FnOnce() -> Result<(), Error>) -> wt_status {
    match catch_unwind(AssertUnwindSafe(body)) {
        Ok(Ok(())) => wt_status::WT_OK,
        Ok(Err(err)) => report(&err, error_out),
        Err(_) => report(
            &Error::internal("watoots panicked; this is a bug in watoots"),
            error_out,
        ),
    }
}

fn report(err: &Error, error_out: *mut *mut wt_error_t) -> wt_status {
    let code = wt_status::from(err.kind());
    if !error_out.is_null() {
        let message = CString::new(err.message())
            .unwrap_or_else(|_| c"error message contained a NUL byte".to_owned());
        // SAFETY: checked non-null; the caller owns the result and frees it
        // with wt_error_delete.
        unsafe { *error_out = Box::into_raw(Box::new(wt_error_t { code, message })) };
    }
    code
}

/// Borrow a C string, rejecting NULL and invalid UTF-8 rather than guessing.
unsafe fn borrow_str<'a>(ptr: *const c_char, what: &str) -> Result<&'a str, Error> {
    if ptr.is_null() {
        return Err(Error::invalid_argument(format!("{what} must not be NULL")));
    }
    unsafe { CStr::from_ptr(ptr) }
        .to_str()
        .map_err(|_| Error::invalid_argument(format!("{what} is not valid UTF-8")))
}

fn into_c_string(text: &str) -> Result<*mut c_char, Error> {
    CString::new(text)
        .map(CString::into_raw)
        .map_err(|_| Error::internal("value contained a NUL byte"))
}

// ---------------------------------------------------------------------------
// Version and status
// ---------------------------------------------------------------------------

/// Library version, e.g. `"0.0.0"`. Never NULL; static storage.
#[unsafe(no_mangle)]
pub extern "C" fn wt_version_string() -> *const c_char {
    c"0.0.0".as_ptr()
}

/// Stable spelling of a status code, e.g. `"WT_ERR_MANIFEST"`. Never NULL.
#[unsafe(no_mangle)]
pub extern "C" fn wt_status_name(status: wt_status) -> *const c_char {
    match status {
        wt_status::WT_OK => c"WT_OK",
        wt_status::WT_ERR_INVALID_ARGUMENT => c"WT_ERR_INVALID_ARGUMENT",
        wt_status::WT_ERR_NOT_FOUND => c"WT_ERR_NOT_FOUND",
        wt_status::WT_ERR_MANIFEST => c"WT_ERR_MANIFEST",
        wt_status::WT_ERR_PERMISSION_DENIED => c"WT_ERR_PERMISSION_DENIED",
        wt_status::WT_ERR_LOAD => c"WT_ERR_LOAD",
        wt_status::WT_ERR_TRAP => c"WT_ERR_TRAP",
        wt_status::WT_ERR_LIMIT_EXCEEDED => c"WT_ERR_LIMIT_EXCEEDED",
        wt_status::WT_ERR_INTERNAL => c"WT_ERR_INTERNAL",
    }
    .as_ptr()
}

// ---------------------------------------------------------------------------
// Strings and errors
// ---------------------------------------------------------------------------

/// Copy a C string into one watoots owns, for returning from a host function.
///
/// Returns NULL if `text` is NULL or contains a NUL byte.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wt_string_new(text: *const c_char) -> *mut c_char {
    if text.is_null() {
        return ptr::null_mut();
    }
    let borrowed = unsafe { CStr::from_ptr(text) };
    CString::new(borrowed.to_bytes()).map_or(ptr::null_mut(), CString::into_raw)
}

/// Free a string produced by this library. NULL is ignored.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wt_string_delete(text: *mut c_char) {
    if !text.is_null() {
        drop(unsafe { CString::from_raw(text) });
    }
}

/// Build an error to return from a host function.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wt_error_new(code: wt_status, message: *const c_char) -> *mut wt_error_t {
    let message = if message.is_null() {
        CString::default()
    } else {
        CString::from(unsafe { CStr::from_ptr(message) })
    };
    Box::into_raw(Box::new(wt_error_t { code, message }))
}

/// Free an error. NULL is ignored.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wt_error_delete(error: *mut wt_error_t) {
    if !error.is_null() {
        drop(unsafe { Box::from_raw(error) });
    }
}

/// The error's status code.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wt_error_code(error: *const wt_error_t) -> wt_status {
    if error.is_null() {
        return wt_status::WT_ERR_INVALID_ARGUMENT;
    }
    unsafe { (*error).code }
}

/// The error's message. Borrowed; valid until the error is deleted.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wt_error_message(error: *const wt_error_t) -> *const c_char {
    if error.is_null() {
        return c"".as_ptr();
    }
    unsafe { (*error).message.as_ptr() }
}

// ---------------------------------------------------------------------------
// Host builder
// ---------------------------------------------------------------------------

/// Start building a host.
#[unsafe(no_mangle)]
pub extern "C" fn wt_host_builder_new() -> *mut wt_host_builder_t {
    Box::into_raw(Box::new(wt_host_builder_t {
        inner: Some(Host::builder()),
    }))
}

/// Free a builder that was never built. NULL is ignored.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wt_host_builder_delete(builder: *mut wt_host_builder_t) {
    if !builder.is_null() {
        drop(unsafe { Box::from_raw(builder) });
    }
}

/// Apply `edit` to the builder in place.
unsafe fn with_builder(
    builder: *mut wt_host_builder_t,
    edit: impl FnOnce(HostBuilder) -> Result<HostBuilder, Error>,
) -> Result<(), Error> {
    if builder.is_null() {
        return Err(Error::invalid_argument("builder must not be NULL"));
    }
    let slot = unsafe { &mut (*builder).inner };
    let current = slot
        .take()
        .ok_or_else(|| Error::invalid_argument("this builder was already built"))?;
    *slot = Some(edit(current)?);
    Ok(())
}

/// Read the manifest from a TOML file.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wt_host_builder_manifest_from_file(
    builder: *mut wt_host_builder_t,
    path: *const c_char,
    error_out: *mut *mut wt_error_t,
) -> wt_status {
    guard(error_out, || {
        let path = unsafe { borrow_str(path, "path") }?;
        unsafe { with_builder(builder, |b| b.manifest_from_file(path)) }
    })
}

/// Set the manifest from TOML text.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wt_host_builder_manifest_from_string(
    builder: *mut wt_host_builder_t,
    toml: *const c_char,
    error_out: *mut *mut wt_error_t,
) -> wt_status {
    guard(error_out, || {
        let toml = unsafe { borrow_str(toml, "toml") }?;
        let manifest = Manifest::parse(toml)?;
        unsafe { with_builder(builder, |b| Ok(b.manifest(manifest))) }
    })
}

/// Define a `${name}` substitution for manifest paths.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wt_host_builder_var(
    builder: *mut wt_host_builder_t,
    name: *const c_char,
    value: *const c_char,
    error_out: *mut *mut wt_error_t,
) -> wt_status {
    guard(error_out, || {
        let name = unsafe { borrow_str(name, "name") }?;
        let value = unsafe { borrow_str(value, "value") }?;
        unsafe { with_builder(builder, |b| Ok(b.var(name, value))) }
    })
}

/// Cache compiled components under this directory. Must be a trusted path.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wt_host_builder_cache_dir(
    builder: *mut wt_host_builder_t,
    dir: *const c_char,
    error_out: *mut *mut wt_error_t,
) -> wt_status {
    guard(error_out, || {
        let dir = unsafe { borrow_str(dir, "dir") }?;
        unsafe { with_builder(builder, |b| Ok(b.cache_dir(dir))) }
    })
}

/// Declare that the application serves this interface without supplying it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wt_host_builder_provide_interface(
    builder: *mut wt_host_builder_t,
    iface: *const c_char,
    error_out: *mut *mut wt_error_t,
) -> wt_status {
    guard(error_out, || {
        let interface = unsafe { borrow_str(iface, "interface") }?;
        unsafe { with_builder(builder, |b| Ok(b.provide_interface(interface))) }
    })
}

/// A C function pointer plus its userdata, promised to be thread-safe.
struct Callback {
    func: wt_host_func_t,
    userdata: *mut c_void,
}

// SAFETY: watoots does not serialise calls into a host function, so the C side
// has to make its callback and userdata thread-safe. This is stated in the
// documentation for `wt_host_func_t`; the alternative is a lock on every
// import crossing, which would tax every single-threaded embedder to protect
// a case they do not have.
unsafe impl Send for Callback {}
unsafe impl Sync for Callback {}

/// Serve one function of one interface to plugins.
///
/// `interface` must be spelled as the component imports it, version included.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wt_host_builder_host_func(
    builder: *mut wt_host_builder_t,
    iface: *const c_char,
    func: *const c_char,
    implementation: wt_host_func_t,
    userdata: *mut c_void,
    error_out: *mut *mut wt_error_t,
) -> wt_status {
    guard(error_out, || {
        let interface = unsafe { borrow_str(iface, "interface") }?.to_string();
        let func = unsafe { borrow_str(func, "func") }?.to_string();
        if implementation.is_none() {
            return Err(Error::invalid_argument("implementation must not be NULL"));
        }
        let callback = Callback {
            func: implementation,
            userdata,
        };

        unsafe {
            with_builder(builder, move |b| {
                Ok(b.host_func(&interface, &func, move |call| {
                    dispatch_host_func(&callback, call)
                }))
            })
        }
    })
}

/// Bridge one import crossing out to C and back.
fn dispatch_host_func(callback: &Callback, call: &HostCall<'_>) -> Result<Vec<Val>, Error> {
    let encoded: Vec<CString> = call
        .args()
        .iter()
        .map(|value| {
            let text = watoots::to_wave(value)?;
            CString::new(text).map_err(|_| Error::internal("WAVE text contained a NUL byte"))
        })
        .collect::<Result<_, Error>>()?;
    let argv: Vec<*const c_char> = encoded.iter().map(|s| s.as_ptr()).collect();

    let mut result: *mut c_char = ptr::null_mut();
    let mut error: *mut wt_error_t = ptr::null_mut();

    // SAFETY: the pointers live for the duration of the call, and the callback
    // is non-null because it was checked at registration.
    let status = unsafe {
        (callback.func.expect("checked at registration"))(
            callback.userdata,
            argv.as_ptr(),
            argv.len(),
            &raw mut result,
            &raw mut error,
        )
    };

    if status != wt_status::WT_OK {
        let message = if error.is_null() {
            format!("host function failed with {status:?}")
        } else {
            unsafe { (*error).message.to_string_lossy().into_owned() }
        };
        unsafe { wt_error_delete(error) };
        unsafe { wt_string_delete(result) };
        return Err(Error::new(status_to_kind(status), message));
    }
    unsafe { wt_error_delete(error) };

    if result.is_null() {
        return Ok(Vec::new());
    }

    let text = unsafe { CStr::from_ptr(result) }
        .to_str()
        .map_err(|_| Error::internal("host function returned invalid UTF-8"))
        .map(str::to_string);
    unsafe { wt_string_delete(result) };
    let text = text?;

    let Some(ty) = call.result_types().first() else {
        return Err(Error::invalid_argument(format!(
            "host function returned {text:?} but the world declares no result"
        )));
    };
    Ok(vec![watoots::from_wave(ty, &text)?])
}

fn status_to_kind(status: wt_status) -> ErrorKind {
    match status {
        wt_status::WT_ERR_INVALID_ARGUMENT => ErrorKind::InvalidArgument,
        wt_status::WT_ERR_NOT_FOUND => ErrorKind::NotFound,
        wt_status::WT_ERR_MANIFEST => ErrorKind::Manifest,
        wt_status::WT_ERR_PERMISSION_DENIED => ErrorKind::PermissionDenied,
        wt_status::WT_ERR_LOAD => ErrorKind::Load,
        wt_status::WT_ERR_TRAP => ErrorKind::Trap,
        wt_status::WT_ERR_LIMIT_EXCEEDED => ErrorKind::LimitExceeded,
        _ => ErrorKind::Internal,
    }
}

/// Build the host. The builder is consumed but must still be freed with
/// [`wt_host_builder_delete`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wt_host_builder_build(
    builder: *mut wt_host_builder_t,
    host_out: *mut *mut wt_host_t,
    error_out: *mut *mut wt_error_t,
) -> wt_status {
    guard(error_out, || {
        if builder.is_null() || host_out.is_null() {
            return Err(Error::invalid_argument(
                "builder and host_out must not be NULL",
            ));
        }
        let inner = unsafe { (*builder).inner.take() }
            .ok_or_else(|| Error::invalid_argument("this builder was already built"))?;
        let host = inner.build()?;
        unsafe { *host_out = Box::into_raw(Box::new(wt_host_t { inner: host })) };
        Ok(())
    })
}

// ---------------------------------------------------------------------------
// Host
// ---------------------------------------------------------------------------

/// Free a host. NULL is ignored.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wt_host_delete(host: *mut wt_host_t) {
    if !host.is_null() {
        drop(unsafe { Box::from_raw(host) });
    }
}

unsafe fn borrow_host<'a>(host: *const wt_host_t) -> Result<&'a Host, Error> {
    if host.is_null() {
        return Err(Error::invalid_argument("host must not be NULL"));
    }
    Ok(unsafe { &(*host).inner })
}

fn wrap_plugin(plugin: Plugin, plugin_out: *mut *mut wt_plugin_t) -> Result<(), Error> {
    let name = CString::new(plugin.name())
        .map_err(|_| Error::internal("plugin name contained a NUL byte"))?;
    unsafe {
        *plugin_out = Box::into_raw(Box::new(wt_plugin_t {
            inner: plugin,
            name,
        }));
    }
    Ok(())
}

/// Load a component from a file.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wt_host_load(
    host: *const wt_host_t,
    path: *const c_char,
    plugin_out: *mut *mut wt_plugin_t,
    error_out: *mut *mut wt_error_t,
) -> wt_status {
    guard(error_out, || {
        let host = unsafe { borrow_host(host) }?;
        let path = unsafe { borrow_str(path, "path") }?;
        if plugin_out.is_null() {
            return Err(Error::invalid_argument("plugin_out must not be NULL"));
        }
        wrap_plugin(host.load(path)?, plugin_out)
    })
}

/// Load a component already in memory.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wt_host_load_binary(
    host: *const wt_host_t,
    name: *const c_char,
    wasm: *const u8,
    wasm_len: usize,
    plugin_out: *mut *mut wt_plugin_t,
    error_out: *mut *mut wt_error_t,
) -> wt_status {
    guard(error_out, || {
        let host = unsafe { borrow_host(host) }?;
        let name = unsafe { borrow_str(name, "name") }?;
        if wasm.is_null() || plugin_out.is_null() {
            return Err(Error::invalid_argument(
                "wasm and plugin_out must not be NULL",
            ));
        }
        let bytes = unsafe { std::slice::from_raw_parts(wasm, wasm_len) };
        wrap_plugin(host.load_binary(name, bytes)?, plugin_out)
    })
}

/// Describe what a component would be granted, without instantiating it.
///
/// Writes a human-readable report to `report_out`; free it with
/// [`wt_string_delete`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wt_host_inspect(
    host: *const wt_host_t,
    wasm: *const u8,
    wasm_len: usize,
    report_out: *mut *mut c_char,
    error_out: *mut *mut wt_error_t,
) -> wt_status {
    guard(error_out, || {
        let host = unsafe { borrow_host(host) }?;
        if wasm.is_null() || report_out.is_null() {
            return Err(Error::invalid_argument(
                "wasm and report_out must not be NULL",
            ));
        }
        let bytes = unsafe { std::slice::from_raw_parts(wasm, wasm_len) };
        let report = host.inspect(bytes)?;
        unsafe { *report_out = into_c_string(&report.describe())? };
        Ok(())
    })
}

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

/// Free a plugin. NULL is ignored.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wt_plugin_delete(plugin: *mut wt_plugin_t) {
    if !plugin.is_null() {
        drop(unsafe { Box::from_raw(plugin) });
    }
}

/// The plugin's name. Borrowed; valid until the plugin is deleted.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wt_plugin_name(plugin: *const wt_plugin_t) -> *const c_char {
    if plugin.is_null() {
        return c"".as_ptr();
    }
    unsafe { (*plugin).name.as_ptr() }
}

/// Call an exported function with WAVE-encoded arguments.
///
/// On success `*result_out` is either NULL, when the function returns nothing,
/// or a WAVE string to free with [`wt_string_delete`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wt_plugin_call(
    plugin: *mut wt_plugin_t,
    export: *const c_char,
    args: *const *const c_char,
    args_len: usize,
    result_out: *mut *mut c_char,
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

        let mut borrowed = Vec::with_capacity(args_len);
        for index in 0..args_len {
            let raw = unsafe { *args.add(index) };
            borrowed.push(unsafe { borrow_str(raw, "argument") }?);
        }

        let plugin = unsafe { &mut (*plugin).inner };
        let results = plugin.call_wave(export, &borrowed)?;

        unsafe { *result_out = ptr::null_mut() };
        match results.len() {
            0 => Ok(()),
            1 => {
                unsafe { *result_out = into_c_string(&results[0])? };
                Ok(())
            }
            // WIT 0.2 functions return at most one value, so this is a signal
            // that the world moved on rather than something to paper over.
            more => Err(Error::internal(format!(
                "{export} returned {more} values; the C API expects at most one"
            ))),
        }
    })
}
