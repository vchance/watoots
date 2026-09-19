// A reader for the slice of WAVE the example worlds' values are written in.
//
// Shared by the C++ example hosts and nothing else. It lives in examples/ and
// not in the library on purpose: results cross the C API as text, and what an
// application actually has to do to read one should be visible in the examples
// rather than hidden behind a dependency. Two hosts needing the same two
// hundred lines is the reason it is a header rather than a copy.

#ifndef WATOOTS_EXAMPLES_WAVE_READER_HPP_
#define WATOOTS_EXAMPLES_WAVE_READER_HPP_

#include <cstddef>
#include <cstdint>
#include <string>
#include <string_view>
#include <utility>

#include "watoots.hpp"

namespace wave {

// A value the plugin returned that this host cannot read. Deliberately not
// WT_ERR_INVALID_ARGUMENT: the argument was ours, the answer is theirs, and
// conflating the two sends the reader to the wrong file.
wt::Error Unreadable(const std::string& message) {
  return {WT_ERR_INTERNAL, "the plugin's answer did not parse: " + message};
}

// ---------------------------------------------------------------------------
// Reading WAVE
// ---------------------------------------------------------------------------

// A reader for the slice of WAVE this world's values are written in: records,
// lists, variants, enums, strings and unsigned integers.
//
// It exists because results cross the C API as text and the C API has no typed
// accessor -- `wt_plugin_call` hands back a string, and a host that wants a
// pixel out of it has to parse one. Written by hand and kept small rather than
// pulled in from anywhere, so that what an application actually has to do is
// visible instead of hidden behind a dependency.
//
// The first failure is sticky and every later call is a no-op, so callers read
// as a description of the grammar rather than as a chain of error handling. A
// loop must still test `Ok()`, because a reader that has failed reports every
// `Peek` as false and would otherwise spin.
class WaveReader {
 public:
  explicit WaveReader(std::string_view text) : text_(text) {}

  // A reader borrows; it does not own. Building one from a temporary string
  // -- `WaveReader(value->value_or(""))` -- leaves it reading a dead stack
  // slot after the semicolon, which worked in a plain build and was a
  // stack-use-after-scope the first time the previewer ran under
  // AddressSanitizer. This overload makes that a compile error instead. Name
  // the string, then build the reader over it.
  explicit WaveReader(std::string&&) = delete;

  [[nodiscard]] bool Ok() const { return !failed_; }

  // The first failure. Default-constructed, and meaningless, while `Ok()`.
  [[nodiscard]] const wt::Error& Failure() const { return failure_; }

  // Record a failure of the caller's own, in the same channel.
  void Reject(const std::string& message) {
    if (Ok()) {
      failure_ = Unreadable(message);
      failed_ = true;
    }
  }

  // Whether the next non-space character is `expected`, without consuming it.
  bool Peek(char expected) {
    SkipSpace();
    return Ok() && at_ < text_.size() && text_.at(at_) == expected;
  }

  bool Take(char expected) {
    if (!Peek(expected)) {
      return false;
    }
    ++at_;
    return true;
  }

  void Expect(char expected) {
    if (!Take(expected)) {
      Reject(std::string("expected '") + expected + "' " + Here());
    }
  }

  // Every character of `expected`, in order.
  void Expect(std::string_view expected) {
    for (const char character : expected) {
      Expect(character);
    }
  }

  // A bare word: an enum case, a variant case, or a record field name.
  std::string Word() {
    SkipSpace();
    const size_t start = at_;
    while (Ok() && at_ < text_.size() && IsWordCharacter(text_.at(at_))) {
      ++at_;
    }
    if (at_ == start) {
      Reject("expected a name " + Here());
      return {};
    }
    return std::string(text_.substr(start, at_ - start));
  }

  uint32_t Number() {
    SkipSpace();
    const size_t start = at_;
    uint64_t value = 0;
    while (Ok() && at_ < text_.size() && text_.at(at_) >= '0' &&
           text_.at(at_) <= '9') {
      value = (value * 10) + static_cast<uint64_t>(text_.at(at_) - '0');
      if (value > UINT32_MAX) {
        Reject("a number too large for u32 " + Here());
        return 0;
      }
      ++at_;
    }
    if (at_ == start) {
      Reject("expected a number " + Here());
      return 0;
    }
    return static_cast<uint32_t>(value);
  }

  // The same, for a `u64` payload such as `truncated(N)`.
  uint64_t Number64() {
    SkipSpace();
    const size_t start = at_;
    uint64_t value = 0;
    while (Ok() && at_ < text_.size() && text_.at(at_) >= '0' &&
           text_.at(at_) <= '9') {
      const auto digit = static_cast<uint64_t>(text_.at(at_) - '0');
      if (value > (UINT64_MAX - digit) / 10) {
        Reject("a number too large for u64 " + Here());
        return 0;
      }
      value = (value * 10) + digit;
      ++at_;
    }
    if (at_ == start) {
      Reject("expected a number " + Here());
      return 0;
    }
    return value;
  }

  // A quoted string, with its escapes resolved.
  std::string Text() {
    Expect('"');
    std::string out;
    while (Ok() && at_ < text_.size()) {
      const char character = text_.at(at_);
      ++at_;
      if (character == '"') {
        return out;
      }
      out += character == '\\' ? Escape() : std::string(1, character);
    }
    Reject("a string was never closed");
    return {};
  }

  // Consume whatever value starts here without interpreting it, so a record
  // can carry a field this host has never heard of without becoming
  // unreadable. Iterative rather than recursive: the nesting depth here comes
  // from a plugin, and a parser that recursed on it would be a stack overflow
  // with extra steps.
  void SkipValue() {
    size_t depth = 0;
    SkipSpace();
    while (Ok() && at_ < text_.size()) {
      const char character = text_.at(at_);
      if (character == '"') {
        Text();
      } else if (character == '{' || character == '[' || character == '(') {
        ++depth;
        ++at_;
      } else if (character == '}' || character == ']' || character == ')') {
        if (depth == 0) {
          return;  // the closer belongs to whoever called us
        }
        --depth;
        ++at_;
      } else if (character == ',' && depth == 0) {
        return;
      } else {
        ++at_;
      }
      if (depth == 0 && AtValueEnd()) {
        return;
      }
    }
  }

 private:
  static bool IsWordCharacter(char character) {
    return (character >= 'a' && character <= 'z') ||
           (character >= 'A' && character <= 'Z') ||
           (character >= '0' && character <= '9') || character == '-' ||
           character == '_' || character == '%';
  }

  bool AtValueEnd() {
    SkipSpace();
    if (at_ >= text_.size()) {
      return true;
    }
    const char character = text_.at(at_);
    return character == ',' || character == '}' || character == ']' ||
           character == ')';
  }

  void SkipSpace() {
    while (at_ < text_.size() &&
           (text_.at(at_) == ' ' || text_.at(at_) == '\t' ||
            text_.at(at_) == '\n' || text_.at(at_) == '\r')) {
      ++at_;
    }
  }

  // The body of one backslash escape, the backslash not yet consumed.
  std::string Escape() {
    if (at_ >= text_.size()) {
      Reject("a string ended inside an escape");
      return {};
    }
    const char character = text_.at(at_);
    ++at_;
    switch (character) {
      case 'n':
        return "\n";
      case 'r':
        return "\r";
      case 't':
        return "\t";
      case 'u':
        return CodePoint();
      default:
        // \" \\ \' and anything else: the character itself.
        return {character};
    }
  }

  // `\u{2014}`, encoded as UTF-8. A failure `reason` is prose written by a
  // guest, so it can legitimately contain one.
  std::string CodePoint() {
    Expect('{');
    uint32_t value = 0;
    size_t digits = 0;
    while (Ok() && at_ < text_.size() && text_.at(at_) != '}') {
      const int digit = HexDigit(text_.at(at_));
      ++at_;
      ++digits;
      if (digit < 0 || digits > 6) {
        Reject("a bad \\u{...} escape " + Here());
        return {};
      }
      value = (value << 4U) | static_cast<uint32_t>(digit);
    }
    Expect('}');
    return Ok() ? Utf8(value) : std::string{};
  }

  static int HexDigit(char character) {
    if (character >= '0' && character <= '9') {
      return character - '0';
    }
    if (character >= 'a' && character <= 'f') {
      return (character - 'a') + 10;
    }
    if (character >= 'A' && character <= 'F') {
      return (character - 'A') + 10;
    }
    return -1;
  }

  static std::string Utf8(uint32_t code_point) {
    std::string out;
    const auto byte = [&out](uint32_t value) {
      out += static_cast<char>(static_cast<unsigned char>(value));
    };
    if (code_point < 0x80) {
      byte(code_point);
    } else if (code_point < 0x800) {
      byte(0xC0 | (code_point >> 6U));
      byte(0x80 | (code_point & 0x3FU));
    } else if (code_point < 0x10000) {
      byte(0xE0 | (code_point >> 12U));
      byte(0x80 | ((code_point >> 6U) & 0x3FU));
      byte(0x80 | (code_point & 0x3FU));
    } else {
      byte(0xF0 | (code_point >> 18U));
      byte(0x80 | ((code_point >> 12U) & 0x3FU));
      byte(0x80 | ((code_point >> 6U) & 0x3FU));
      byte(0x80 | (code_point & 0x3FU));
    }
    return out;
  }

  // A short excerpt, so an error is actionable without quoting fifteen
  // megabytes of pixels back at the reader.
  [[nodiscard]] std::string Here() const {
    constexpr size_t kExcerpt = 40;
    const size_t start = at_ > kExcerpt / 2 ? at_ - (kExcerpt / 2) : 0;
    return "at offset " + std::to_string(at_) + ", near \"" +
           std::string(text_.substr(start, kExcerpt)) + "\"";
  }

  std::string_view text_;
  size_t at_ = 0;
  wt::Error failure_;
  bool failed_ = false;
};

}  // namespace wave

#endif  // WATOOTS_EXAMPLES_WAVE_READER_HPP_
