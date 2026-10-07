#ifndef WATOOTS_HPP_
#define WATOOTS_HPP_

#include <functional>
#include <memory>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <utility>
#include <vector>
#include <version>

#include "watoots.h"

// Google style forbids exceptions and the C API is status-code based, so every
// fallible operation returns Result<T>. Where the consumer's standard library
// has std::expected (C++23), Result *is* std::expected; otherwise it is a small
// shim exposing the same subset, so downstream code reads identically at either
// standard. That is what lets the shipped header hold a C++20 floor (ADR-0003)
// without giving up the C++23 spelling.
//
// One hazard comes with that: at C++20 Result is the shim and at C++23 it is
// std::expected, so they are *different types*. A project that compiles some
// translation units at C++20 and others at C++23 and passes a Result between
// them has an ODR violation. The fix is to define WATOOTS_FORCE_RESULT_SHIM
// everywhere, which selects the shim regardless of standard. The test suite
// builds all three configurations.
#if defined(__cpp_lib_expected) && __cpp_lib_expected >= 202202L && \
    !defined(WATOOTS_FORCE_RESULT_SHIM)
#define WATOOTS_RESULT_USES_STD_EXPECTED 1
#else
#define WATOOTS_RESULT_USES_STD_EXPECTED 0
#endif

#if WATOOTS_RESULT_USES_STD_EXPECTED
#include <expected>
#else
#include <type_traits>
#include <variant>
#endif

namespace wt {

// An error crossing the C boundary: a stable status code plus a human-readable
// message. Accessors are CamelCase -- Google permits variable-style names for
// accessors, but uniform CamelCase is the half clang-tidy can enforce.
class Error {
 public:
  Error() = default;
  Error(wt_status code, std::string message)
      : code_(code), message_(std::move(message)) {}

  [[nodiscard]] wt_status Code() const noexcept { return code_; }
  [[nodiscard]] const std::string& Message() const& noexcept {
    return message_;
  }

 private:
  wt_status code_ = WT_ERR_INTERNAL;
  std::string message_;
};

#if WATOOTS_RESULT_USES_STD_EXPECTED

template <class T>
using Result = std::expected<T, Error>;
using std::unexpected;

#else

// NOLINTBEGIN(readability-identifier-naming, google-explicit-constructor)
// These names and the implicit value constructor mirror std::expected exactly.
// Diverging would defeat the point: code written against one path must compile
// unchanged against the other.

template <class E>
class unexpected {
 public:
  explicit unexpected(E error) : error_(std::move(error)) {}

  [[nodiscard]] const E& error() const& noexcept { return error_; }
  [[nodiscard]] E&& error() && noexcept { return std::move(error_); }

 private:
  E error_;
};

template <class E>
unexpected(E) -> unexpected<E>;

// Subset of std::expected<T, Error>. value() on an error Result is a
// programming error -- check has_value() first. Both paths throw on that
// misuse, but with different exception types, so never rely on it.
template <class T>
class Result {
 public:
  using value_type = T;
  using error_type = Error;

  Result()
    requires std::is_default_constructible_v<T>
  = default;

  Result(T value) : storage_(std::in_place_index<0>, std::move(value)) {}

  template <class E>
  Result(unexpected<E> unex)
      : storage_(std::in_place_index<1>, std::move(unex).error()) {}

  [[nodiscard]] bool has_value() const noexcept {
    return storage_.index() == 0;
  }
  explicit operator bool() const noexcept { return has_value(); }

  [[nodiscard]] T& value() & { return std::get<0>(storage_); }
  [[nodiscard]] const T& value() const& { return std::get<0>(storage_); }
  [[nodiscard]] T&& value() && { return std::get<0>(std::move(storage_)); }

  [[nodiscard]] const Error& error() const& { return std::get<1>(storage_); }
  [[nodiscard]] Error&& error() && { return std::get<1>(std::move(storage_)); }

  [[nodiscard]] T& operator*() & noexcept { return *std::get_if<0>(&storage_); }
  [[nodiscard]] const T& operator*() const& noexcept {
    return *std::get_if<0>(&storage_);
  }

  [[nodiscard]] T* operator->() noexcept { return std::get_if<0>(&storage_); }
  [[nodiscard]] const T* operator->() const noexcept {
    return std::get_if<0>(&storage_);
  }

  template <class U>
  [[nodiscard]] T value_or(U&& fallback) const& {
    return has_value() ? std::get<0>(storage_)
                       : static_cast<T>(std::forward<U>(fallback));
  }

 private:
  std::variant<T, Error> storage_;
};

// std::expected<void, E>: success carries nothing.
template <>
class Result<void> {
 public:
  using value_type = void;
  using error_type = Error;

  Result() = default;

  template <class E>
  Result(unexpected<E> unex) : error_(std::move(unex).error()), failed_(true) {}

  [[nodiscard]] bool has_value() const noexcept { return !failed_; }
  explicit operator bool() const noexcept { return has_value(); }

  void value() const noexcept {}

  [[nodiscard]] const Error& error() const& noexcept { return error_; }
  [[nodiscard]] Error&& error() && noexcept { return std::move(error_); }

 private:
  Error error_;
  bool failed_ = false;
};

// NOLINTEND(readability-identifier-naming, google-explicit-constructor)

#endif  // WATOOTS_RESULT_USES_STD_EXPECTED

namespace internal {

// Move-only owner of an opaque wt_* handle. Every type in the C++ API is one of
// these plus typed methods, so the lifetime rules live in exactly one place --
// which is what makes testing them, and running them under ASan, worth doing.
template <class T, class Deleter>
class OwnedHandle {
 public:
  OwnedHandle() = default;
  explicit OwnedHandle(T* raw) noexcept : raw_(raw) {}

  OwnedHandle(const OwnedHandle&) = delete;
  OwnedHandle& operator=(const OwnedHandle&) = delete;

  OwnedHandle(OwnedHandle&& other) noexcept
      : raw_(std::exchange(other.raw_, nullptr)) {}

  OwnedHandle& operator=(OwnedHandle&& other) noexcept {
    if (this != &other) {
      Reset(std::exchange(other.raw_, nullptr));
    }
    return *this;
  }

  ~OwnedHandle() { Reset(); }

  [[nodiscard]] T* Get() const noexcept { return raw_; }
  explicit operator bool() const noexcept { return raw_ != nullptr; }

  // Hands ownership back to the caller; no delete happens.
  [[nodiscard]] T* Release() noexcept { return std::exchange(raw_, nullptr); }

  void Reset(T* raw = nullptr) noexcept {
    if (raw_ != nullptr) {
      Deleter{}(raw_);
    }
    raw_ = raw;
  }

 private:
  T* raw_ = nullptr;
};

}  // namespace internal

// ---------------------------------------------------------------------------
// The API
// ---------------------------------------------------------------------------

namespace internal {

// One deleter per handle type. Each is the C++ half of a `wt_*_new` /
// `wt_*_delete` pair, so ownership is stated once and never repeated.
struct HostDeleter {
  void operator()(wt_host_t* host) const noexcept { wt_host_delete(host); }
};
struct PluginDeleter {
  void operator()(wt_plugin_t* plugin) const noexcept {
    wt_plugin_delete(plugin);
  }
};
struct BuilderDeleter {
  void operator()(wt_host_builder_t* builder) const noexcept {
    wt_host_builder_delete(builder);
  }
};

// Take ownership of an error the C API produced, or synthesise one when the
// call reported a failure without a message.
inline Error TakeError(wt_status status, wt_error_t* raw) {
  if (raw == nullptr) {
    return {status, "watoots reported a failure with no message"};
  }
  Error error(wt_error_code(raw), wt_error_message(raw));
  wt_error_delete(raw);
  return error;
}

}  // namespace internal

/// A WAVE-encoded value, or nothing when a function returns no value.
using Value = std::optional<std::string>;

/// A borrowed typed WIT value: the view every accessor works on.
///
/// Valid for as long as the `Val` it was read out of. Each `As*` accessor
/// answers `nullopt` when the value is not that kind, so a host that reads a
/// field it expected and gets nothing back has learned the plugin returned
/// something other than its world declares -- and has not read a byte of it.
///
/// Borrowed strings are copied into `std::string_view`s that point into the
/// value, so they too live only as long as it does.
class ValRef {
 public:
  explicit ValRef(const wt_val_t* raw) noexcept : raw_(raw) {}

  [[nodiscard]] const wt_val_t* Raw() const noexcept { return raw_; }

  [[nodiscard]] wt_val_kind Kind() const noexcept {
    return wt_val_kind_of(raw_);
  }

  [[nodiscard]] std::optional<bool> AsBool() const noexcept {
    bool out = false;
    return wt_val_as_bool(raw_, &out) ? std::optional{out} : std::nullopt;
  }

  /// Any signed integer, or an unsigned one below 2^63.
  [[nodiscard]] std::optional<int64_t> AsS64() const noexcept {
    int64_t out = 0;
    return wt_val_as_s64(raw_, &out) ? std::optional{out} : std::nullopt;
  }

  /// Any unsigned integer, or a signed one that is not negative.
  [[nodiscard]] std::optional<uint64_t> AsU64() const noexcept {
    uint64_t out = 0;
    return wt_val_as_u64(raw_, &out) ? std::optional{out} : std::nullopt;
  }

  /// An `f32` or `f64`.
  [[nodiscard]] std::optional<double> AsF64() const noexcept {
    double out = 0;
    return wt_val_as_f64(raw_, &out) ? std::optional{out} : std::nullopt;
  }

  [[nodiscard]] std::optional<char32_t> AsChar() const noexcept {
    uint32_t out = 0;
    return wt_val_as_char(raw_, &out)
               ? std::optional{static_cast<char32_t>(out)}
               : std::nullopt;
  }

  [[nodiscard]] std::optional<std::string_view> AsString() const noexcept {
    const char* data = nullptr;
    size_t len = 0;
    if (!wt_val_as_string(raw_, &data, &len)) {
      return std::nullopt;
    }
    return std::string_view(data, len);
  }

  /// A `list<u8>`, copied out. `nullopt` if any item is not a `u8`.
  [[nodiscard]] std::optional<std::vector<uint8_t>> AsBytes() const {
    size_t len = 0;
    if (!wt_val_as_bytes(raw_, nullptr, 0, &len)) {
      return std::nullopt;
    }
    std::vector<uint8_t> out(len);
    wt_val_as_bytes(raw_, out.data(), out.size(), &len);
    return out;
  }

  /// Items of a list or tuple, fields of a record, or flags set. Zero for
  /// anything else.
  [[nodiscard]] size_t Size() const noexcept { return wt_val_len(raw_); }

  /// Item `index` of a list or tuple.
  [[nodiscard]] std::optional<ValRef> Item(size_t index) const noexcept {
    return Wrap(wt_val_item(raw_, index));
  }

  /// A record field by name.
  [[nodiscard]] std::optional<ValRef> Field(const char* name) const noexcept {
    return Wrap(wt_val_field(raw_, name));
  }

  /// Record field `index`, in declaration order, and its name.
  [[nodiscard]] std::optional<ValRef> FieldAt(size_t index) const noexcept {
    return Wrap(wt_val_field_at(raw_, index));
  }
  [[nodiscard]] std::optional<std::string_view> FieldName(
      size_t index) const noexcept {
    size_t len = 0;
    const char* name = wt_val_field_name(raw_, index, &len);
    if (name == nullptr) {
      return std::nullopt;
    }
    return std::string_view(name, len);
  }

  /// The case name of a variant or enum.
  [[nodiscard]] std::optional<std::string_view> Case() const noexcept {
    const char* name = nullptr;
    size_t len = 0;
    if (!wt_val_case(raw_, &name, &len)) {
      return std::nullopt;
    }
    return std::string_view(name, len);
  }

  /// The payload of a variant case, an option's `some`, or either side of a
  /// result; `nullopt` when there is none.
  [[nodiscard]] std::optional<ValRef> Payload() const noexcept {
    return Wrap(wt_val_payload(raw_));
  }

  /// Whether a result is `ok` or an option is `some`.
  [[nodiscard]] std::optional<bool> IsOk() const noexcept {
    bool out = false;
    return wt_val_is_ok(raw_, &out) ? std::optional{out} : std::nullopt;
  }

  /// The name of set flag `index` of a flags value.
  [[nodiscard]] std::optional<std::string_view> FlagAt(
      size_t index) const noexcept {
    size_t len = 0;
    const char* name = wt_val_flag_at(raw_, index, &len);
    if (name == nullptr) {
      return std::nullopt;
    }
    return std::string_view(name, len);
  }

  /// The value as WAVE text, for a log line or a trace. `nullopt` for a value
  /// WAVE cannot spell.
  [[nodiscard]] std::optional<std::string> ToWave() const {
    char* text = wt_val_to_wave(raw_);
    if (text == nullptr) {
      return std::nullopt;
    }
    std::string out(text);
    wt_string_delete(text);
    return out;
  }

 protected:
  static std::optional<ValRef> Wrap(const wt_val_t* raw) noexcept {
    if (raw == nullptr) {
      return std::nullopt;
    }
    return ValRef(raw);
  }

  /// For `Val`, which owns what this views and has to be able to let go of it.
  void SetRaw(const wt_val_t* raw) noexcept { raw_ = raw; }

 private:
  const wt_val_t* raw_;
};

/// An owned typed WIT value: what the typed `Plugin::Call` takes and returns.
///
/// Build one with the static constructors below, read it through the `ValRef`
/// accessors it inherits. A constructor that takes child values moves them in,
/// so a `Val` is only ever inside one other `Val`. Move-only; `Clone()` is the
/// explicit deep copy.
///
/// This is the path for payloads that are bytes rather than words: `Bytes()`
/// copies a buffer once, where the WAVE path would render every byte as text
/// and parse it back on the other side.
class Val : public ValRef {
 public:
  /// Takes ownership of a value from the C API.
  explicit Val(wt_val_t* raw) noexcept : ValRef(raw) {}
  ~Val() { wt_val_delete(Mutable()); }

  Val(const Val&) = delete;
  Val& operator=(const Val&) = delete;
  Val(Val&& other) noexcept : ValRef(other.Raw()) { other.SetRaw(nullptr); }
  Val& operator=(Val&& other) noexcept {
    if (this != &other) {
      wt_val_delete(Mutable());
      SetRaw(other.Raw());
      other.SetRaw(nullptr);
    }
    return *this;
  }

  static Val Bool(bool value) { return Val(wt_val_bool(value)); }
  static Val S8(int8_t value) { return Val(wt_val_s8(value)); }
  static Val U8(uint8_t value) { return Val(wt_val_u8(value)); }
  static Val S16(int16_t value) { return Val(wt_val_s16(value)); }
  static Val U16(uint16_t value) { return Val(wt_val_u16(value)); }
  static Val S32(int32_t value) { return Val(wt_val_s32(value)); }
  static Val U32(uint32_t value) { return Val(wt_val_u32(value)); }
  static Val S64(int64_t value) { return Val(wt_val_s64(value)); }
  static Val U64(uint64_t value) { return Val(wt_val_u64(value)); }
  static Val F32(float value) { return Val(wt_val_f32(value)); }
  static Val F64(double value) { return Val(wt_val_f64(value)); }
  /// Null if `codepoint` is not a Unicode scalar value.
  static Val Char(char32_t codepoint) {
    return Val(wt_val_char(static_cast<uint32_t>(codepoint)));
  }
  /// Null if `text` is not UTF-8.
  static Val String(std::string_view text) {
    return Val(wt_val_string(text.data(), text.size()));
  }
  /// A `list<u8>`.
  static Val Bytes(std::span<const uint8_t> bytes) {
    return Val(wt_val_bytes(bytes.data(), bytes.size()));
  }
  static Val Bytes(std::span<const std::byte> bytes) {
    // Viewing bytes as bytes; see ADR-0003 on why this is spelled out here.
    // NOLINTNEXTLINE(cppcoreguidelines-pro-type-reinterpret-cast)
    return Val(wt_val_bytes(reinterpret_cast<const uint8_t*>(bytes.data()),
                            bytes.size()));
  }
  static Val List(std::vector<Val> items) {
    std::vector<wt_val_t*> raw = Release(std::move(items));
    return Val(wt_val_list(raw.data(), raw.size()));
  }
  static Val Tuple(std::vector<Val> items) {
    std::vector<wt_val_t*> raw = Release(std::move(items));
    return Val(wt_val_tuple(raw.data(), raw.size()));
  }
  /// Fields in the WIT declaration order.
  static Val Record(std::vector<std::pair<const char*, Val>> fields) {
    std::vector<const char*> names;
    std::vector<wt_val_t*> raw;
    names.reserve(fields.size());
    raw.reserve(fields.size());
    for (auto& [name, value] : fields) {
      names.push_back(name);
      raw.push_back(value.Release());
    }
    return Val(wt_val_record(names.data(), raw.data(), raw.size()));
  }
  static Val Variant(const char* case_name, std::optional<Val> payload) {
    return Val(wt_val_variant(case_name, ReleaseOptional(std::move(payload))));
  }
  static Val Enum(const char* case_name) { return Val(wt_val_enum(case_name)); }
  static Val Some(Val value) { return Val(wt_val_option(value.Release())); }
  static Val None() { return Val(wt_val_option(nullptr)); }
  static Val Ok(std::optional<Val> payload = std::nullopt) {
    return Val(wt_val_ok(ReleaseOptional(std::move(payload))));
  }
  static Val Err(std::optional<Val> payload = std::nullopt) {
    return Val(wt_val_err(ReleaseOptional(std::move(payload))));
  }
  static Val Flags(std::span<const char* const> names) {
    return Val(wt_val_flags(names.data(), names.size()));
  }

  /// False when a constructor was handed something it could not build from:
  /// a string that is not UTF-8, a surrogate, a null child.
  [[nodiscard]] bool Valid() const noexcept { return Raw() != nullptr; }
  explicit operator bool() const noexcept { return Valid(); }

  [[nodiscard]] Val Clone() const { return Val(wt_val_clone(Raw())); }

  /// Give up ownership. The caller frees the result with `wt_val_delete`, or
  /// hands it to a C constructor that does.
  [[nodiscard]] wt_val_t* Release() noexcept {
    wt_val_t* raw = Mutable();
    SetRaw(nullptr);
    return raw;
  }

 private:
  // The C API hands out `wt_val_t*` and takes `const wt_val_t*`; `ValRef`
  // stores the const form so the accessors are shared. Ownership is the only
  // reason to want it back the other way.
  [[nodiscard]] wt_val_t* Mutable() const noexcept {
    // NOLINTNEXTLINE(cppcoreguidelines-pro-type-const-cast)
    return const_cast<wt_val_t*>(Raw());
  }

  static std::vector<wt_val_t*> Release(std::vector<Val> items) {
    std::vector<wt_val_t*> raw;
    raw.reserve(items.size());
    for (Val& item : items) {
      raw.push_back(item.Release());
    }
    return raw;
  }

  static wt_val_t* ReleaseOptional(std::optional<Val> value) noexcept {
    return value.has_value() ? value->Release() : nullptr;
  }
};

/// A function the application serves to plugins.
///
/// Arguments and the result are WAVE text. Return an error to make the guest's
/// call fail. A `std::function` rather than a plain pointer so it can capture
/// the application state it needs to answer; [`HostBuilder::Build`] hands
/// ownership of the callables to the [`Host`], which outlives every call into
/// them.
///
/// Must not throw: it is invoked from C, where unwinding is undefined
/// behaviour.
using HostFunction =
    std::function<Result<Value>(std::span<const std::string_view> args)>;

/// Where a plugin's `wasi:logging` messages go.
///
/// `context` and `message` are untrusted: they come from the plugin. Copy what
/// you keep, and never pass either as a format string.
///
/// No timestamp is handed over -- stamp it here, from the host clock. A
/// guest-supplied one would defeat the pinned wall clock and make a recording
/// unreplayable. Whether a plugin may log at all, and from which level up, is
/// the manifest's `permissions.logging`, not this callback's business.
///
/// Must not throw: it is invoked from C, where unwinding is undefined
/// behaviour.
using LogFunction = std::function<void(
    wt_log_level level, std::string_view context, std::string_view message)>;

/// One authorisation decision, with its strings owned.
///
/// The C event borrows every string for the duration of the callback; this
/// copies them, so a host can queue a decision and write it out later. Read
/// `kind` first: a field that does not apply to it is empty or unspecified, and
/// the comments on `wt_audit_event_t` say which kinds set what.
///
/// **Nothing here is an argument value or a log message body.** A suppressed
/// line is reported as a suppression at a level; the line is not in it. That is
/// what makes an audit record safe to keep and to paste into an issue, and it
/// is why this is not the trace hook. See ADR-0011.
struct AuditEvent {
  wt_audit_kind kind = WT_AUDIT_LOADED;
  /// The whole decision rendered as one line, ready to forward to a logger.
  std::string line;
  std::string plugin;
  /// SHA-256 of the component bytes, for the load and reload kinds.
  std::string sha256;
  /// The import this is about, or the one that decided a refusal. Named
  /// `import_name` because `import` is a contextual keyword in C++20 modules.
  std::string import_name;
  /// What that import needs, e.g. `"network"`.
  std::string requirement;
  /// The manifest key an operator would edit, e.g. `"permissions.net"`.
  std::string grant_key;
  /// Why a load or reload was refused. May span several lines; `line` keeps
  /// only the first.
  std::string reason;
  /// The manifest key of the ceiling that was spent.
  std::string limit_key;
  wt_audit_verdict verdict = WT_AUDIT_GRANTED;
  wt_ceiling ceiling = WT_CEILING_FUEL;
  wt_log_level level = WT_LOG_TRACE;
  /// False when the guest named a `level` case this build does not define.
  bool level_known = false;
  wt_log_level ceiling_level = WT_LOG_TRACE;
  uint64_t imports = 0;
  uint64_t reloads = 0;
};

/// Observes every authorisation decision the host makes.
///
/// Off unless you install one: an application with no hook gets no audit trail.
///
/// Must not throw: it is invoked from C, where unwinding is undefined
/// behaviour. It may be entered from any thread — a `wasi:logging` verdict is
/// reached inside the guest's own call.
using AuditFunction = std::function<void(const AuditEvent& event)>;

/// One per-WIT-function row of a profile, with its names owned.
///
/// The C row borrows its strings from the plugin and is invalidated by the
/// next profile taken on it; this copies them, so a caller can keep a profile
/// around and compare it with a later one.
struct FunctionProfile {
  wt_function_kind kind = WT_FUNCTION_EXPORT;
  /// Interface name, version included. Empty for an export.
  std::string interface_name;
  std::string func;
  uint64_t calls = 0;
  uint64_t wall_nanos = 0;
  uint64_t guest_nanos = 0;
  uint64_t host_nanos = 0;
  uint64_t marshalling_nanos = 0;
  /// Of that, WAVE text conversion. Zero for an import.
  uint64_t wave_nanos = 0;
};

/// Where a plugin's time has gone, split at the host/guest boundary.
///
/// `guest_nanos` and `host_nanos` are measured at the exact transitions the
/// engine reports. `marshalling_nanos` is derived -- what is left of
/// `wall_nanos` after the other two -- so it holds the canonical ABI's copying
/// and watoots' own dispatch alike. It is a diagnostic, not an accounting
/// identity. See ADR-0009.
struct PluginProfile {
  uint64_t calls = 0;
  uint64_t wall_nanos = 0;
  uint64_t guest_nanos = 0;
  uint64_t host_nanos = 0;
  uint64_t marshalling_nanos = 0;
  /// Of that, WAVE text conversion. Every call through this API pays it.
  uint64_t wave_nanos = 0;
  /// Exports first, then imports, each group sorted by name. Only the host
  /// functions watoots installed itself appear, so these do not add up to
  /// `host_nanos` -- the `wasi:` interfaces are in the bucket and not in the
  /// list.
  std::vector<FunctionProfile> functions;
};

/// A loaded plugin.
class Plugin {
 public:
  Plugin() = default;

  /// The name this plugin was loaded under. Empty for a default-constructed
  /// Plugin.
  [[nodiscard]] std::string_view Name() const noexcept {
    return handle_ ? wt_plugin_name(handle_.Get()) : std::string_view{};
  }

  /// Whether this holds a plugin.
  explicit operator bool() const noexcept { return static_cast<bool>(handle_); }

  /// What the host has observed about this plugin since it was loaded.
  ///
  /// Measured at the boundary, never reported by the guest, which is the
  /// distinction ADR-0006 draws in declining to build guest-emitted metrics.
  [[nodiscard]] Result<wt_plugin_stats_t> Stats() const {
    wt_plugin_stats_t stats{};
    wt_error_t* error = nullptr;
    const wt_status status = wt_plugin_stats(handle_.Get(), &stats, &error);
    if (status != WT_OK) {
      return unexpected(internal::TakeError(status, error));
    }
    return stats;
  }

  /// Where this plugin's time has gone, split at the boundary.
  ///
  /// The sibling of `Stats`: same rationale, different question -- "where"
  /// rather than "how much". Needs `HostBuilder::Profile`, and fails with
  /// `WT_ERR_INVALID_ARGUMENT` without it, because a page of zeroes is a worse
  /// answer than being told the feature is off.
  ///
  /// Not const: taking a profile refreshes the rows the C API borrows names
  /// from, which invalidates any row read earlier.
  [[nodiscard]] Result<PluginProfile> Profile() {
    wt_plugin_profile_t totals{};
    wt_error_t* error = nullptr;
    wt_status status = wt_plugin_profile(handle_.Get(), &totals, &error);
    if (status != WT_OK) {
      return unexpected(internal::TakeError(status, error));
    }

    PluginProfile profile;
    profile.calls = totals.calls;
    profile.wall_nanos = totals.wall_nanos;
    profile.guest_nanos = totals.guest_nanos;
    profile.host_nanos = totals.host_nanos;
    profile.marshalling_nanos = totals.marshalling_nanos;
    profile.wave_nanos = totals.wave_nanos;
    profile.functions.reserve(totals.function_count);

    for (uint64_t index = 0; index < totals.function_count; ++index) {
      wt_function_profile_t row{};
      status = wt_plugin_profile_function(handle_.Get(), index, &row, &error);
      if (status != WT_OK) {
        return unexpected(internal::TakeError(status, error));
      }
      profile.functions.push_back(FunctionProfile{
          .kind = row.kind,
          .interface_name = row.iface == nullptr ? "" : row.iface,
          .func = row.func == nullptr ? "" : row.func,
          .calls = row.calls,
          .wall_nanos = row.wall_nanos,
          .guest_nanos = row.guest_nanos,
          .host_nanos = row.host_nanos,
          .marshalling_nanos = row.marshalling_nanos,
          .wave_nanos = row.wave_nanos,
      });
    }
    return profile;
  }

  /// Write the sampled guest profile to `path`, as Firefox Profiler JSON.
  ///
  /// Needs `HostBuilder::ProfileGuestSamples`. The profiler is consumed, so
  /// sampling stops here and a second call fails. Load the file at
  /// https://profiler.firefox.com/.
  Result<void> WriteGuestProfile(const std::string& path) {
    wt_error_t* error = nullptr;
    const wt_status status =
        wt_plugin_write_guest_profile(handle_.Get(), path.c_str(), &error);
    if (status != WT_OK) {
      return unexpected(internal::TakeError(status, error));
    }
    return {};
  }

  /// Replace this plugin's code with new bytes, keeping its name and handle.
  ///
  /// Reload is `Host::LoadBinary` plus a state handoff, not a cheaper path
  /// that skips the checks: the same manifest, the same import intersection,
  /// the same refusal. New bytes must not acquire a capability by arriving as
  /// an update.
  ///
  /// **A failure leaves this plugin running the code it was already running**
  /// -- the replacement is built to completion before it takes over -- with
  /// one exception, a trap inside `save-state`, after which Wasmtime will not
  /// re-enter the instance. See `wt_plugin_reload` for the state hooks and for
  /// what happens to the counters.
  [[nodiscard]] Result<wt_reload_report_t> Reload(
      std::span<const std::byte> wasm) {
    wt_reload_report_t report{};
    wt_error_t* error = nullptr;
    const wt_status status = wt_plugin_reload(
        handle_.Get(),
        reinterpret_cast<const uint8_t*>(wasm.data()),  // NOLINT
        wasm.size(), &report, &error);
    if (status != WT_OK) {
      return unexpected(internal::TakeError(status, error));
    }
    return report;
  }

  /// Replace this plugin's code with a component the signature vouches for.
  ///
  /// The same check `Host::LoadBinarySigned` runs. A reload re-verifies on
  /// purpose: the moment the code changes is the moment publisher identity
  /// matters most.
  [[nodiscard]] Result<wt_reload_report_t> ReloadSigned(
      std::span<const std::byte> wasm, std::span<const std::byte> signature) {
    wt_reload_report_t report{};
    wt_error_t* error = nullptr;
    const wt_status status = wt_plugin_reload_signed(
        handle_.Get(),
        reinterpret_cast<const uint8_t*>(wasm.data()),  // NOLINT
        wasm.size(),
        reinterpret_cast<const uint8_t*>(signature.data()),  // NOLINT
        signature.size(), &report, &error);
    if (status != WT_OK) {
      return unexpected(internal::TakeError(status, error));
    }
    return report;
  }

  /// Replace this plugin's code with a component read from `path`.
  ///
  /// As `Reload`, and additionally re-points `${plugin_dir}` at the directory
  /// the replacement came from. The plugin keeps the name it was loaded under.
  [[nodiscard]] Result<wt_reload_report_t> ReloadFile(const std::string& path) {
    wt_reload_report_t report{};
    wt_error_t* error = nullptr;
    const wt_status status =
        wt_plugin_reload_file(handle_.Get(), path.c_str(), &report, &error);
    if (status != WT_OK) {
      return unexpected(internal::TakeError(status, error));
    }
    return report;
  }

  /// Call an exported function with WAVE-encoded arguments.
  Result<Value> Call(const std::string& export_name,
                     std::span<const std::string> args) {
    std::vector<const char*> argv;
    argv.reserve(args.size());
    for (const std::string& arg : args) {
      argv.push_back(arg.c_str());
    }

    char* result = nullptr;
    wt_error_t* error = nullptr;
    const wt_status status =
        wt_plugin_call(handle_.Get(), export_name.c_str(), argv.data(),
                       argv.size(), &result, &error);
    if (status != WT_OK) {
      return unexpected(internal::TakeError(status, error));
    }
    if (result == nullptr) {
      return Value{};
    }
    Value value{std::string(result)};
    wt_string_delete(result);
    return value;
  }

  /// Convenience for a call with no arguments.
  Result<Value> Call(const std::string& export_name) {
    return Call(export_name, std::span<const std::string>{});
  }

  /// Call an exported function with typed arguments.
  ///
  /// The same call as the WAVE overload -- same limits, same trace, same
  /// audit -- with no text in between, which is what makes a `list<u8>` of a
  /// few million pixels affordable. `nullopt` when the function returns no
  /// value.
  Result<std::optional<Val>> Call(const std::string& export_name,
                                  std::span<const Val> args) {
    std::vector<const wt_val_t*> argv;
    argv.reserve(args.size());
    for (const Val& arg : args) {
      argv.push_back(arg.Raw());
    }

    wt_val_t* result = nullptr;
    wt_error_t* error = nullptr;
    const wt_status status =
        wt_plugin_call_vals(handle_.Get(), export_name.c_str(), argv.data(),
                            argv.size(), &result, &error);
    if (status != WT_OK) {
      return unexpected(internal::TakeError(status, error));
    }
    if (result == nullptr) {
      return std::optional<Val>{};
    }
    return std::optional<Val>{Val(result)};
  }

 private:
  friend class Host;
  explicit Plugin(wt_plugin_t* raw) noexcept : handle_(raw) {}

  internal::OwnedHandle<wt_plugin_t, internal::PluginDeleter> handle_;
};

/// A configured host: an engine plus the policy its plugins run under.
class Host {
 public:
  Host() = default;

  /// Whether this holds a host.
  explicit operator bool() const noexcept { return static_cast<bool>(handle_); }

  /// Load a component from a file.
  [[nodiscard]] Result<Plugin> Load(const std::string& path) const {
    wt_plugin_t* plugin = nullptr;
    wt_error_t* error = nullptr;
    const wt_status status =
        wt_host_load(handle_.Get(), path.c_str(), &plugin, &error);
    if (status != WT_OK) {
      return unexpected(internal::TakeError(status, error));
    }
    return Plugin(plugin);
  }

  /// Load a component already in memory.
  [[nodiscard]] Result<Plugin> LoadBinary(
      const std::string& name, std::span<const std::byte> wasm) const {
    wt_plugin_t* plugin = nullptr;
    wt_error_t* error = nullptr;
    const wt_status status = wt_host_load_binary(
        handle_.Get(), name.c_str(),
        reinterpret_cast<const uint8_t*>(wasm.data()),  // NOLINT
        wasm.size(), &plugin, &error);
    if (status != WT_OK) {
      return unexpected(internal::TakeError(status, error));
    }
    return Plugin(plugin);
  }

  /// Load a component from memory with the signature that vouches for it.
  ///
  /// `signature` is base64, as `cosign sign-blob --output-signature` writes it,
  /// and is checked against the manifest's `signature.keys` before the
  /// component is compiled. With no keys configured it is ignored rather than
  /// rejected, so it can be passed unconditionally.
  [[nodiscard]] Result<Plugin> LoadBinarySigned(
      const std::string& name, std::span<const std::byte> wasm,
      std::span<const std::byte> signature) const {
    wt_plugin_t* plugin = nullptr;
    wt_error_t* error = nullptr;
    const wt_status status = wt_host_load_binary_signed(
        handle_.Get(), name.c_str(),
        reinterpret_cast<const uint8_t*>(wasm.data()),  // NOLINT
        wasm.size(),
        reinterpret_cast<const uint8_t*>(signature.data()),  // NOLINT
        signature.size(), &plugin, &error);
    if (status != WT_OK) {
      return unexpected(internal::TakeError(status, error));
    }
    return Plugin(plugin);
  }

  /// Describe what a component would be granted, without instantiating it.
  [[nodiscard]] Result<std::string> Inspect(
      std::span<const std::byte> wasm) const {
    char* report = nullptr;
    wt_error_t* error = nullptr;
    const wt_status status = wt_host_inspect(
        handle_.Get(),
        reinterpret_cast<const uint8_t*>(wasm.data()),  // NOLINT
        wasm.size(), &report, &error);
    if (status != WT_OK) {
      return unexpected(internal::TakeError(status, error));
    }
    std::string text(report == nullptr ? "" : report);
    wt_string_delete(report);
    return text;
  }

  /// Every import and its decision, one per line: the detail behind `Inspect`.
  [[nodiscard]] Result<std::string> InspectImports(
      std::span<const std::byte> wasm) const {
    char* report = nullptr;
    wt_error_t* error = nullptr;
    const wt_status status = wt_host_inspect_imports(
        handle_.Get(),
        reinterpret_cast<const uint8_t*>(wasm.data()),  // NOLINT
        wasm.size(), &report, &error);
    if (status != WT_OK) {
      return unexpected(internal::TakeError(status, error));
    }
    std::string text(report == nullptr ? "" : report);
    wt_string_delete(report);
    return text;
  }

  /// Check that a component implements a world.
  ///
  /// `Inspect` asks whether a plugin wants anything it should not; this asks
  /// whether it provides what you are about to call. `wit` is a WIT file, a
  /// directory containing one, or a wasm-encoded WIT package. Omit `world`
  /// when the package declares exactly one.
  [[nodiscard]] Result<void> CheckTargets(
      std::span<const std::byte> wasm, const std::string& wit,
      const std::optional<std::string>& world = std::nullopt) const {
    wt_error_t* error = nullptr;
    const wt_status status = wt_host_check_targets(
        handle_.Get(),
        reinterpret_cast<const uint8_t*>(wasm.data()),  // NOLINT
        wasm.size(), wit.c_str(), world ? world->c_str() : nullptr, &error);
    if (status != WT_OK) {
      return unexpected(internal::TakeError(status, error));
    }
    return {};
  }

 private:
  friend class HostBuilder;
  Host(wt_host_t* raw, std::vector<std::unique_ptr<HostFunction>> functions,
       std::unique_ptr<LogFunction> log_sink,
       std::unique_ptr<AuditFunction> audit_hook)
      : handle_(raw),
        functions_(std::move(functions)),
        log_sink_(std::move(log_sink)),
        audit_hook_(std::move(audit_hook)) {}

  internal::OwnedHandle<wt_host_t, internal::HostDeleter> handle_;
  // The C API holds raw pointers to these, so they must outlive the host.
  std::vector<std::unique_ptr<HostFunction>> functions_;
  std::unique_ptr<LogFunction> log_sink_;
  std::unique_ptr<AuditFunction> audit_hook_;
};

/// Adapts a `HostFunction` to the C callback signature.
///
/// Declared `extern "C"` because it is called from C; a C++ function pointer is
/// not required to be usable there even when it happens to work.
extern "C" inline wt_status WatootsHostFuncTrampoline(  // NOLINT
    void* userdata, const char* const* args, size_t args_len, char** result_out,
    wt_error_t** error_out) {
  auto* function = static_cast<HostFunction*>(userdata);
  std::vector<std::string_view> views;
  views.reserve(args_len);
  for (size_t index = 0; index < args_len; ++index) {
    views.emplace_back(args[index]);  // NOLINT
  }

  Result<Value> outcome = (*function)(views);
  if (!outcome.has_value()) {
    if (error_out != nullptr) {
      *error_out = wt_error_new(outcome.error().Code(),
                                outcome.error().Message().c_str());
    }
    return outcome.error().Code();
  }
  if (outcome.value().has_value() && result_out != nullptr) {
    *result_out = wt_string_new(outcome.value()->c_str());
  }
  return WT_OK;
}

/// Adapts a `LogFunction` to the C sink signature. See above for why this is
/// `extern "C"`.
extern "C" inline void WatootsLogSinkTrampoline(  // NOLINT
    void* userdata, wt_log_level level, const char* context,
    const char* message) {
  auto* sink = static_cast<LogFunction*>(userdata);
  (*sink)(level, context == nullptr ? "" : context,
          message == nullptr ? "" : message);
}

/// Adapts an `AuditFunction` to the C hook signature. See above for why this is
/// `extern "C"`.
extern "C" inline void WatootsAuditHookTrampoline(  // NOLINT
    void* userdata, const char* line, const wt_audit_event_t* event) {
  auto* hook = static_cast<AuditFunction*>(userdata);
  if (event == nullptr) {
    return;
  }

  // Every string is borrowed for the duration of this call, so each one is
  // copied here rather than handed on.
  auto owned = [](const char* text) -> std::string {
    return text == nullptr ? std::string{} : std::string(text);
  };

  AuditEvent decision;
  decision.kind = event->kind;
  decision.line = owned(line);
  decision.plugin = owned(event->plugin);
  decision.sha256 = owned(event->sha256);
  decision.import_name = owned(event->import);
  decision.requirement = owned(event->requirement);
  decision.grant_key = owned(event->grant_key);
  decision.reason = owned(event->reason);
  decision.limit_key = owned(event->limit_key);
  decision.verdict = event->verdict;
  decision.ceiling = event->ceiling;
  decision.level = event->level;
  decision.level_known = event->level_known;
  decision.ceiling_level = event->ceiling_level;
  decision.imports = event->imports;
  decision.reloads = event->reloads;
  (*hook)(decision);
}

/// Builds a [`Host`].
class HostBuilder {
 public:
  HostBuilder() : handle_(wt_host_builder_new()) {}

  /// Read the manifest from a TOML file.
  Result<void> ManifestFromFile(const std::string& path) {
    return Apply(wt_host_builder_manifest_from_file, path);
  }

  /// Set the manifest from TOML text.
  Result<void> ManifestFromString(const std::string& toml) {
    return Apply(wt_host_builder_manifest_from_string, toml);
  }

  /// Cache compiled components under this directory. Must be trusted: entries
  /// are machine code loaded without re-validation. A host already reuses a
  /// component it compiled earlier in the same run, so this is what makes the
  /// first load of the *next* run cheap.
  Result<void> CacheDir(const std::string& dir) {
    return Apply(wt_host_builder_cache_dir, dir);
  }

  /// Declare that the application serves this interface.
  Result<void> ProvideInterface(const std::string& iface) {
    return Apply(wt_host_builder_provide_interface, iface);
  }

  /// Define a `${name}` substitution for manifest paths.
  Result<void> Var(const std::string& name, const std::string& value) {
    wt_error_t* error = nullptr;
    const wt_status status =
        wt_host_builder_var(handle_.Get(), name.c_str(), value.c_str(), &error);
    if (status != WT_OK) {
      return unexpected(internal::TakeError(status, error));
    }
    return {};
  }

  /// Serve one function of one interface to plugins.
  ///
  /// `iface` must be spelled as the component imports it, version included.
  Result<void> HostFunc(const std::string& iface, const std::string& func,
                        HostFunction implementation) {
    auto owned = std::make_unique<HostFunction>(std::move(implementation));
    wt_error_t* error = nullptr;
    const wt_status status = wt_host_builder_host_func(
        handle_.Get(), iface.c_str(), func.c_str(), WatootsHostFuncTrampoline,
        owned.get(), &error);
    if (status != WT_OK) {
      return unexpected(internal::TakeError(status, error));
    }
    functions_.push_back(std::move(owned));
    return {};
  }

  /// Receive the plugin's `wasi:logging` messages.
  ///
  /// The manifest decides whether a plugin may log at all and from which level
  /// up; registering a sink only says where the messages that survive that
  /// ceiling go. Calling this twice replaces the first sink.
  Result<void> LogSink(LogFunction implementation) {
    auto owned = std::make_unique<LogFunction>(std::move(implementation));
    wt_error_t* error = nullptr;
    const wt_status status = wt_host_builder_log_sink(
        handle_.Get(), WatootsLogSinkTrampoline, owned.get(), &error);
    if (status != WT_OK) {
      return unexpected(internal::TakeError(status, error));
    }
    log_sink_ = std::move(owned);
    return {};
  }

  /// Observe every authorisation decision: what a plugin was permitted to do,
  /// and what it was refused.
  ///
  /// The sibling of a trace recorder and deliberately not the same hook. A
  /// trace answers *what happened* and carries the argument values to prove it;
  /// this answers *what was allowed*, and carries names and verdicts only. A
  /// suppressed log line is reported as a suppression at a level; the line
  /// itself is not in it, which is what makes an audit record safe to keep.
  ///
  /// **Off unless you call this.** An application with no hook installed gets
  /// no audit trail. Calling this twice replaces the first hook.
  Result<void> AuditHook(AuditFunction implementation) {
    auto owned = std::make_unique<AuditFunction>(std::move(implementation));
    wt_error_t* error = nullptr;
    const wt_status status = wt_host_builder_audit_hook(
        handle_.Get(), WatootsAuditHookTrampoline, owned.get(), &error);
    if (status != WT_OK) {
      return unexpected(internal::TakeError(status, error));
    }
    audit_hook_ = std::move(owned);
    return {};
  }

  /// Split every plugin's time into guest, host-call and marshalling.
  ///
  /// Opt-in: a call hook then fires on every host/guest transition. Read the
  /// result with `Plugin::Profile`. Refused alongside a trace recorder, since
  /// profiling changes timing and the recording would not reproduce.
  Result<void> Profile() {
    wt_error_t* error = nullptr;
    const wt_status status = wt_host_builder_profile(handle_.Get(), &error);
    if (status != WT_OK) {
      return unexpected(internal::TakeError(status, error));
    }
    return {};
  }

  /// Also sample guest stacks every `interval_ms`, for a Firefox profile.
  ///
  /// Implies `Profile`. Sampling shares the epoch deadline with
  /// `limits.timeout` and the timeout wins, so this cannot keep a runaway
  /// plugin alive. Write the result with `Plugin::WriteGuestProfile`.
  Result<void> ProfileGuestSamples(uint64_t interval_ms) {
    wt_error_t* error = nullptr;
    const wt_status status = wt_host_builder_profile_guest_samples(
        handle_.Get(), interval_ms, &error);
    if (status != WT_OK) {
      return unexpected(internal::TakeError(status, error));
    }
    return {};
  }

  /// Build the host. The builder is spent afterwards, and the host takes over
  /// keeping the registered host functions and log sink alive.
  Result<Host> Build() {
    wt_host_t* host = nullptr;
    wt_error_t* error = nullptr;
    const wt_status status =
        wt_host_builder_build(handle_.Get(), &host, &error);
    if (status != WT_OK) {
      return unexpected(internal::TakeError(status, error));
    }
    auto functions = std::move(functions_);
    auto log_sink = std::move(log_sink_);
    auto audit_hook = std::move(audit_hook_);
    // Spent, and definitively so. The C layer already refuses a second build,
    // but that invariant lives across the FFI boundary where neither the
    // compiler nor the static analyser can see it -- so leave the vector empty
    // rather than merely moved-from.
    functions_.clear();
    log_sink_.reset();
    audit_hook_.reset();
    return Host(host, std::move(functions), std::move(log_sink),
                std::move(audit_hook));
  }

 private:
  using StringSetter = wt_status (*)(wt_host_builder_t*, const char*,
                                     wt_error_t**);

  Result<void> Apply(StringSetter setter, const std::string& value) {
    wt_error_t* error = nullptr;
    const wt_status status = setter(handle_.Get(), value.c_str(), &error);
    if (status != WT_OK) {
      return unexpected(internal::TakeError(status, error));
    }
    return {};
  }

  internal::OwnedHandle<wt_host_builder_t, internal::BuilderDeleter> handle_;
  std::vector<std::unique_ptr<HostFunction>> functions_;
  std::unique_ptr<LogFunction> log_sink_;
  std::unique_ptr<AuditFunction> audit_hook_;
};

}  // namespace wt

#endif  // WATOOTS_HPP_
