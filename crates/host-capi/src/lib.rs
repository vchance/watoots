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

use watoots::{
    Error, ErrorKind, Host, HostBuilder, HostCall, LogLevel, LogRecord, Manifest, Plugin, Val,
};

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

/// Severity of a `wasi:logging` message.
///
/// The six cases and their order are `wasi:logging@0.1.0-draft`'s own, so the
/// numeric values are stable for as long as that proposal's `enum level` is.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum wt_log_level {
    WT_LOG_TRACE = 0,
    WT_LOG_DEBUG = 1,
    WT_LOG_INFO = 2,
    WT_LOG_WARN = 3,
    WT_LOG_ERROR = 4,
    WT_LOG_CRITICAL = 5,
}

impl From<LogLevel> for wt_log_level {
    fn from(level: LogLevel) -> Self {
        match level {
            LogLevel::Trace => Self::WT_LOG_TRACE,
            LogLevel::Debug => Self::WT_LOG_DEBUG,
            LogLevel::Info => Self::WT_LOG_INFO,
            LogLevel::Warn => Self::WT_LOG_WARN,
            LogLevel::Error => Self::WT_LOG_ERROR,
            LogLevel::Critical => Self::WT_LOG_CRITICAL,
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

/// Where a plugin's `wasi:logging` messages go.
///
/// `context` and `message` are NUL-terminated, borrowed for the duration of the
/// call, and **untrusted**: they come from the plugin. Copy what you keep, and
/// never pass either to a `printf`-family format argument.
///
/// There is no timestamp parameter. A guest-supplied one would defeat the
/// pinned wall clock and make a recording unreplayable, and a host-supplied one
/// would only be a worse version of the stamp your own logging framework
/// applies. Stamp it in the sink.
///
/// Returns nothing: `wasi:logging`'s `log` has no result, so there is no
/// failure for a guest to observe. The sink and `userdata` must be safe to use
/// from any thread, and must not throw — watoots does not serialise calls into
/// it, and unwinding across the boundary is undefined behaviour.
pub type wt_log_sink_t = Option<
    unsafe extern "C" fn(
        userdata: *mut c_void,
        level: wt_log_level,
        context: *const c_char,
        message: *const c_char,
    ),
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
    c"0.1.0".as_ptr()
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

/// The `wasi:logging` spelling of a level, e.g. `"warn"`. Never NULL.
#[unsafe(no_mangle)]
pub extern "C" fn wt_log_level_name(level: wt_log_level) -> *const c_char {
    match level {
        wt_log_level::WT_LOG_TRACE => c"trace",
        wt_log_level::WT_LOG_DEBUG => c"debug",
        wt_log_level::WT_LOG_INFO => c"info",
        wt_log_level::WT_LOG_WARN => c"warn",
        wt_log_level::WT_LOG_ERROR => c"error",
        wt_log_level::WT_LOG_CRITICAL => c"critical",
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

/// A C log sink plus its userdata, promised to be thread-safe.
struct Sink {
    func: wt_log_sink_t,
    userdata: *mut c_void,
}

// SAFETY: identical to `Callback` above, and for the same reason — a sink is
// entered inline on the guest's call, from whatever thread made it.
unsafe impl Send for Sink {}
unsafe impl Sync for Sink {}

/// Receive the plugin's `wasi:logging` messages.
///
/// Whether the interface links at all is the manifest's decision, not this one:
/// `permissions.logging` grants it and sets the level ceiling, and a component
/// importing `wasi:logging` under a manifest that says nothing fails to load
/// however many sinks are registered. Calling this twice replaces the first
/// sink; there is one, and watoots does no fan-out.
///
/// `userdata` must outlive the host built from this builder.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wt_host_builder_log_sink(
    builder: *mut wt_host_builder_t,
    sink: wt_log_sink_t,
    userdata: *mut c_void,
    error_out: *mut *mut wt_error_t,
) -> wt_status {
    guard(error_out, || {
        if sink.is_none() {
            return Err(Error::invalid_argument("sink must not be NULL"));
        }
        let sink = Sink {
            func: sink,
            userdata,
        };

        unsafe {
            with_builder(builder, move |b| {
                Ok(b.log_sink(move |record| dispatch_log(&sink, record)))
            })
        }
    })
}

/// Bridge one log record out to C.
///
/// A NUL byte inside guest text would truncate a C string silently, which is a
/// way for a plugin to hide the second half of what it said. Replace the whole
/// field instead, so the substitution is visible in the log.
fn dispatch_log(sink: &Sink, record: &LogRecord<'_>) {
    let context = CString::new(record.context())
        .unwrap_or_else(|_| c"<context contained a NUL byte>".to_owned());
    let message = CString::new(record.message())
        .unwrap_or_else(|_| c"<message contained a NUL byte>".to_owned());

    // SAFETY: both strings outlive the call, and the sink is non-null because
    // it was checked at registration.
    unsafe {
        (sink.func.expect("checked at registration"))(
            sink.userdata,
            record.level().into(),
            context.as_ptr(),
            message.as_ptr(),
        );
    }
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
/// Answers "what can this plugin do", resolved against the manifest: a granted
/// filesystem names its directories, an interface the application must serve is
/// listed separately from a denial, and a capability granted but never imported
/// is reported. For the raw per-import list, see [`wt_host_inspect_imports`].
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
        let text = report.summarize(&host.manifest().permissions);
        unsafe { *report_out = into_c_string(&text)? };
        Ok(())
    })
}

/// Every import and its decision, one per line.
///
/// The detail behind [`wt_host_inspect`]. Free `report_out` with
/// [`wt_string_delete`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wt_host_inspect_imports(
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

/// Check that a component implements a world.
///
/// `wt_host_inspect` answers "does this plugin ask for anything it should not";
/// this answers "does it provide what I am about to call". `wit` is a path to a
/// WIT file, a directory containing one, or a wasm-encoded WIT package. `world`
/// may be NULL when the package declares exactly one.
///
/// Returns `WT_OK` when the component conforms, and `WT_ERR_LOAD` with a
/// message naming the world when it does not.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wt_host_check_targets(
    host: *const wt_host_t,
    wasm: *const u8,
    wasm_len: usize,
    wit: *const c_char,
    world: *const c_char,
    error_out: *mut *mut wt_error_t,
) -> wt_status {
    guard(error_out, || {
        let host = unsafe { borrow_host(host) }?;
        if wasm.is_null() {
            return Err(Error::invalid_argument("wasm must not be NULL"));
        }
        let wit = unsafe { borrow_str(wit, "wit") }?;
        let world = if world.is_null() {
            None
        } else {
            Some(unsafe { borrow_str(world, "world") }?)
        };
        let bytes = unsafe { std::slice::from_raw_parts(wasm, wasm_len) };
        host.check_targets(bytes, wit, world)
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

/// What the host has observed about a plugin since it was loaded.
///
/// Plain integers, owned by the caller: there is nothing to free. Every field
/// is measured at the boundary rather than reported by the guest, so a plugin
/// can neither forge nor inflate them. See ADR-0006 for why watoots offers
/// these and not metrics a plugin emits about itself.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
#[allow(non_camel_case_types)]
pub struct wt_plugin_stats_t {
    /// Calls that have completed, successfully or not.
    pub calls: u64,
    /// Fuel burned across those calls; zero when the manifest meters none.
    pub fuel_consumed: u64,
    /// Largest linear memory the guest was granted, in bytes.
    pub peak_memory_bytes: u64,
    /// `wasi:logging` messages emitted.
    pub log_messages: u64,
    /// Bytes of log message emitted.
    pub log_bytes: u64,
    /// Imports the component declares.
    pub imports_declared: u64,
    /// How many of those the manifest did not grant.
    pub imports_denied: u64,
}

/// Read a plugin's counters into `stats_out`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wt_plugin_stats(
    plugin: *const wt_plugin_t,
    stats_out: *mut wt_plugin_stats_t,
    error_out: *mut *mut wt_error_t,
) -> wt_status {
    guard(error_out, || {
        if plugin.is_null() || stats_out.is_null() {
            return Err(Error::invalid_argument(
                "plugin and stats_out must not be NULL",
            ));
        }
        let stats = unsafe { (*plugin).inner.stats() };
        unsafe {
            *stats_out = wt_plugin_stats_t {
                calls: stats.calls,
                fuel_consumed: stats.fuel_consumed,
                peak_memory_bytes: stats.peak_memory_bytes,
                log_messages: stats.log_messages,
                log_bytes: stats.log_bytes,
                imports_declared: stats.imports_declared as u64,
                imports_denied: stats.imports_denied as u64,
            };
        }
        Ok(())
    })
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
