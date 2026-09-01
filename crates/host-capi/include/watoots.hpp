#ifndef WATOOTS_HPP_
#define WATOOTS_HPP_

#include <functional>
#include <memory>
#include <optional>
#include <span>
#include <string>
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

 private:
  friend class HostBuilder;
  Host(wt_host_t* raw, std::vector<std::unique_ptr<HostFunction>> functions)
      : handle_(raw), functions_(std::move(functions)) {}

  internal::OwnedHandle<wt_host_t, internal::HostDeleter> handle_;
  // The C API holds raw pointers to these, so they must outlive the host.
  std::vector<std::unique_ptr<HostFunction>> functions_;
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

  /// Cache compiled components under this directory. Must be trusted.
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

  /// Build the host. The builder is spent afterwards, and the host takes over
  /// keeping the registered host functions alive.
  Result<Host> Build() {
    wt_host_t* host = nullptr;
    wt_error_t* error = nullptr;
    const wt_status status =
        wt_host_builder_build(handle_.Get(), &host, &error);
    if (status != WT_OK) {
      return unexpected(internal::TakeError(status, error));
    }
    auto functions = std::move(functions_);
    // Spent, and definitively so. The C layer already refuses a second build,
    // but that invariant lives across the FFI boundary where neither the
    // compiler nor the static analyser can see it -- so leave the vector empty
    // rather than merely moved-from.
    functions_.clear();
    return Host(host, std::move(functions));
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
};

}  // namespace wt

#endif  // WATOOTS_HPP_
