#ifndef WATOOTS_HPP_
#define WATOOTS_HPP_

#include <string>
#include <utility>
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

}  // namespace wt

#endif  // WATOOTS_HPP_
