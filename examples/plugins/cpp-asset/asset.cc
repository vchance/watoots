// A sample watoots plugin: an image-operation pipeline, in C++.
//
//   tools/build-plugins.sh cpp-asset
//
// The same world as `examples/plugins/rust-asset`, and the same *bytes*.
// `examples/wit/asset/asset.wit` says four languages have to agree on every
// output byte; this is the second of them, and the first one written in a
// language whose float-to-integer conversion is undefined behaviour rather
// than a saturating cast.
//
// Where `cpp-lint` shows a C++ guest that needs no capability, this one needs
// exactly one: `lut` opens a file itself, so `fs.read` has to be granted and
// scoped, and a manifest that does not grant it fails the *load*.
//
// # Everything here is a transcription, not a decision
//
// `examples/plugins/rust-asset/src/lib.rs` is the reference implementation and
// documents every arithmetic rule on the function that implements it. This file
// implements the same rules and says so at each site; where C++ offers a
// shorter or prettier spelling that would round differently, the comment says
// which one was rejected and why. The rules, once more:
//
//   grayscale  y = (299*r + 587*g + 114*b + 500) / 1000, integer division
//   invert     255 - c
//   gain       min(255, floor(c * factor + 0.5)) in double
//   resize     sx = dx * src_w / dst_w, integer division; likewise sy
//   lut        out.c = table[in.c].c, per channel, never across channels
//
// The build passes `-ffp-contract=off`. WebAssembly has no fused multiply-add
// instruction, so contraction cannot actually happen here, but `gain` is one
// multiply and one add and the flag makes it impossible rather than merely
// unavailable on this target.
//
// # Memory
//
// `wit-bindgen` generates C, so ownership is manual and stated at each site:
//
//   * Parameters to an exported function belong to the callee. `apply` takes
//     the input image's pixel buffer over for its own use and frees the rest.
//   * Anything returned is freed by the canonical ABI with `free`, so it has to
//     come from `malloc`. A `new[]` there is a heap mismatch.
//   * A zero-length list arrives with a pointer from `cabi_realloc` asked for
//     zero bytes, which returns `(void*)align` -- not a heap pointer. Every
//     free below is therefore guarded on a non-zero length, exactly as the
//     generated helpers are.

#include <array>
#include <cerrno>
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <string>
#include <string_view>
#include <vector>

extern "C" {
#include "bindings/asset_plugin.h"
}

namespace {

// Bytes per pixel. RGB8, no alpha -- the WIT says so.
constexpr uint64_t kChannels = 3;

// Ceiling on a `resize` destination, in bytes. **32 MiB, from the WIT.**
//
// Transcribed from `operation.resize` in `asset.wit`, the same way the Rust
// guest transcribes it. Changing it here would be a silent disagreement with
// the world rather than a tuning decision.
constexpr uint64_t kMaxImageBytes = 32ULL * 1024 * 1024;

// Ceiling on the size of a lookup-table file this plugin will read.
//
// Matches `MAX_LUT_BYTES` in the Rust guest, and for the same reason: a grant
// is a directory, so "what is in luts/" is not a closed set, and reading all of
// whatever someone drops there would exhaust the manifest's memory limit.
constexpr uint64_t kMaxLutBytes = 64ULL * 1024;

// Copy into a WIT string.
//
// The canonical ABI frees everything an export returns -- see `cabi_post_apply`
// in the generated shim, which calls `free` on each string it finds. So the
// allocation has to come from `malloc`.
asset_plugin_string_t Own(std::string_view text) {
  asset_plugin_string_t out{nullptr, 0};
  if (text.empty()) {
    // A zero-length malloc may return nullptr, and the ABI reads a zero length
    // without touching the pointer. Being explicit removes the ambiguity.
    return out;
  }
  auto* buffer = static_cast<uint8_t*>(std::malloc(text.size()));
  if (buffer == nullptr) {
    return out;  // The host sees an empty string rather than a trap.
  }
  std::memcpy(buffer, text.data(), text.size());
  out.ptr = buffer;
  out.len = text.size();
  return out;
}

std::string_view Borrow(const asset_plugin_string_t& text) {
  if (text.len == 0) {
    return {};
  }
  return {reinterpret_cast<const char*>(text.ptr),  // NOLINT: the C ABI's shape
          text.len};
}

// Call the host's `log.emit`.
//
// The string is *lent*, not given. A `string` parameter to an imported function
// stays the caller's: the canonical ABI lowers it to a pointer and a length,
// the host copies during lift, and nothing on the other side frees it. So this
// points straight at the caller's buffer and allocates nothing -- a `malloc`
// here would be a leak on every log line, with no post-return to collect it.
void Emit(watoots_asset_log_level_t level, std::string_view message) {
  asset_plugin_string_t lent{
      // NOLINTNEXTLINE(cppcoreguidelines-pro-type-const-cast): the generated
      // signature takes a non-const pointer and only reads through it.
      const_cast<uint8_t*>(reinterpret_cast<const uint8_t*>(message.data())),
      message.size()};
  watoots_asset_log_emit(level, &lent);
}

// -------------------------------------------------------------------------
// Failures, before they are lowered
// -------------------------------------------------------------------------

// The plugin's own failure type.
//
// `asset_plugin_failure_t` owns `malloc`ed strings, which makes it awkward to
// build, log and then hand over. This carries `std::string`s instead, so
// `Report` can read the reason and `Lower` can copy it exactly once, at the
// boundary -- the same division the Rust guest gets for free from `Failure`
// being an ordinary enum.
struct Failure {
  uint8_t tag = WATOOTS_ASSET_TYPES_FAILURE_MALFORMED;
  watoots_asset_types_operation_kind_t kind = 0;  // `unsupported` only
  std::string path;                               // `unreadable` only
  std::string reason;  // `malformed`'s text, or `unreadable`'s reason
};

Failure Malformed(std::string reason) {
  Failure failure;
  failure.tag = WATOOTS_ASSET_TYPES_FAILURE_MALFORMED;
  failure.reason = std::move(reason);
  return failure;
}

Failure Unreadable(std::string_view path, std::string reason) {
  Failure failure;
  failure.tag = WATOOTS_ASSET_TYPES_FAILURE_UNREADABLE;
  failure.path = std::string(path);
  failure.reason = std::move(reason);
  return failure;
}

// Log a failure on its way out. The mirror of `report` in the Rust guest, down
// to the wording: every case of `failure` carries its own reason, so this line
// is a courtesy to whoever is watching the host's log rather than the only
// channel, and there is exactly one of it per failed call.
void Report(const Failure& failure) {
  std::string detail;
  switch (failure.tag) {
    case WATOOTS_ASSET_TYPES_FAILURE_UNSUPPORTED: {
      // Spelled to match the WIT case names, since that is what a reader of the
      // world will be looking for. Unreachable today -- this plugin implements
      // every case -- and kept so the two guests stay line-for-line comparable.
      const char* name = "grayscale";
      switch (failure.kind) {
        case WATOOTS_ASSET_TYPES_OPERATION_KIND_INVERT:
          name = "invert";
          break;
        case WATOOTS_ASSET_TYPES_OPERATION_KIND_GAIN:
          name = "gain";
          break;
        case WATOOTS_ASSET_TYPES_OPERATION_KIND_RESIZE:
          name = "resize";
          break;
        case WATOOTS_ASSET_TYPES_OPERATION_KIND_LUT:
          name = "lut";
          break;
        default:
          break;
      }
      detail = "unsupported step: " + std::string(name);
      break;
    }
    case WATOOTS_ASSET_TYPES_FAILURE_MALFORMED:
      detail = "malformed input: " + failure.reason;
      break;
    default:
      detail =
          "unreadable lookup table " + failure.path + ": " + failure.reason;
      break;
  }
  Emit(WATOOTS_ASSET_LOG_LEVEL_ERROR, detail);
}

// Copy a `Failure` into the ABI's owned form. Every string here is `malloc`ed,
// because `cabi_post_apply` frees them.
asset_plugin_failure_t Lower(const Failure& failure) {
  asset_plugin_failure_t out{};
  out.tag = failure.tag;
  switch (failure.tag) {
    case WATOOTS_ASSET_TYPES_FAILURE_UNSUPPORTED:
      out.val.unsupported = failure.kind;
      break;
    case WATOOTS_ASSET_TYPES_FAILURE_MALFORMED:
      out.val.malformed = Own(failure.reason);
      break;
    default:
      out.val.unreadable.path = Own(failure.path);
      out.val.unreadable.reason = Own(failure.reason);
      break;
  }
  return out;
}

// -------------------------------------------------------------------------
// The working image
// -------------------------------------------------------------------------

// A pixel buffer this plugin owns, allocated with `malloc` throughout.
//
// `malloc` and not `new[]`, because the buffer's last stop is the canonical
// ABI, which frees it with `free`. The zero-length guard is not defensive
// tidiness: `cabi_realloc(NULL, 0, 1, 0)` returns `(void*)1`, so a zero-length
// list arriving from the host has a pointer that must never reach `free`.
class Pixels {
 public:
  Pixels() = default;
  Pixels(const Pixels&) = delete;
  Pixels& operator=(const Pixels&) = delete;
  Pixels(Pixels&&) = delete;
  Pixels& operator=(Pixels&&) = delete;
  ~Pixels() { Reset(nullptr, 0); }

  // Take over `data`, freeing whatever was held before.
  void Reset(uint8_t* data, size_t len) {
    if (len_ > 0) {
      std::free(data_);
    }
    data_ = data;
    len_ = len;
  }

  // Hand the buffer to the caller; this object no longer owns it.
  uint8_t* Release() {
    uint8_t* out = data_;
    data_ = nullptr;
    len_ = 0;
    return out;
  }

  [[nodiscard]] uint8_t* Data() const { return data_; }
  [[nodiscard]] size_t Size() const { return len_; }

 private:
  uint8_t* data_ = nullptr;
  size_t len_ = 0;
};

struct Image {
  uint32_t width = 0;
  uint32_t height = 0;
  Pixels pixels;
};

// -------------------------------------------------------------------------
// The operations
// -------------------------------------------------------------------------

// Rec. 601 luma, replicated across all three channels.
//
//   y = (299*r + 587*g + 114*b + 500) / 1000
//
// with truncating integer division, in `uint32_t`. Matching the Rust guest,
// which explains the choice at length: the coefficients are the Rec. 601
// weights scaled by 1000, the `+ 500` is half the divisor so the division
// rounds half up, and every term is non-negative so "half up" and "half away
// from zero" are the same rule. The largest numerator is 255500, well inside
// 32 bits.
//
// Deliberately not `0.299f * r + ...`. That is the spelling C++ makes easiest
// and it is the one that disagrees with the other three guests.
void Grayscale(Image* image) {
  uint8_t* pixels = image->pixels.Data();
  for (size_t at = 0; at + 3 <= image->pixels.Size(); at += 3) {
    const uint32_t r = pixels[at];
    const uint32_t g = pixels[at + 1];
    const uint32_t b = pixels[at + 2];
    const auto luma =
        static_cast<uint8_t>((299 * r + 587 * g + 114 * b + 500) / 1000);
    pixels[at] = luma;
    pixels[at + 1] = luma;
    pixels[at + 2] = luma;
  }
}

// `255 - c`, per channel. No rounding: the operation is exact in 8 bits.
void Invert(Image* image) {
  uint8_t* pixels = image->pixels.Data();
  for (size_t at = 0; at < image->pixels.Size(); ++at) {
    pixels[at] = static_cast<uint8_t>(255 - pixels[at]);
  }
}

// Multiply one channel by a factor and clamp to 0..=255.
//
//   out = min(255, floor(c * factor + 0.5))
//
// in `double`, matching the Rust guest exactly. Three things make that
// reproducible, and all three matter more in C++ than they did in Rust:
//
//  1. The factor is an `f32` in the WIT and widening it to `double` is exact,
//     so both guests start from bit-identical operands. The multiply is then
//     one IEEE-754 operation, correctly rounded, and identical everywhere.
//  2. `std::floor(x + 0.5)`, not `std::round`. `std::round` is half-away-from-
//     zero, `std::lrint` follows the rounding mode, and Rust's `f64::round`,
//     JavaScript's `Math.round` and Python's `round` are three further answers.
//     `floor(x + 0.5)` is the same instruction in all four.
//  3. The factor is clamped to 0.0..=4.0 and NaN is treated as 0.0. That is the
//     WIT's rule, stated on `gain.factor`, and this is the guest it was written
//     for: `255.0 * 5.0` converted to `uint8_t` is *undefined behaviour* in
//     C++, where Rust's `as u8` merely saturates. The clamp and the `min` below
//     are what keep the conversion inside the range where it is defined.
void ApplyGain(Image* image, const watoots_asset_types_gain_t& gain) {
  // `gain.factor != gain.factor` is the NaN test that does not need <cmath>'s
  // macro; spelled with `std::isnan` for the reader. Clamping in `float` before
  // widening matches the Rust guest, and 0.0 and 4.0 are exact in both widths
  // so the two orders cannot disagree.
  float clamped = gain.factor;
  if (std::isnan(clamped)) {
    clamped = 0.0F;
  } else if (clamped < 0.0F) {
    clamped = 0.0F;
  } else if (clamped > 4.0F) {
    clamped = 4.0F;
  }
  const auto factor = static_cast<double>(clamped);

  size_t offset = 0;
  switch (gain.channel) {
    case WATOOTS_ASSET_TYPES_CHANNEL_GREEN:
      offset = 1;
      break;
    case WATOOTS_ASSET_TYPES_CHANNEL_BLUE:
      offset = 2;
      break;
    default:
      offset = 0;
      break;
  }

  uint8_t* pixels = image->pixels.Data();
  for (size_t at = 0; at + 3 <= image->pixels.Size(); at += 3) {
    const double scaled =
        std::floor(static_cast<double>(pixels[at + offset]) * factor + 0.5);
    // `scaled` is >= 0 because factor and the sample both are, and `fmin` caps
    // it at 255, so the conversion is defined. Without the cap, a factor of 4
    // on a bright pixel reaches 1020 and the conversion would be UB.
    pixels[at + offset] = static_cast<uint8_t>(std::fmin(scaled, 255.0));
  }
}

// Nearest neighbour, top-left biased. Every rule is `operation.resize` in
// `asset.wit`; this implements the world rather than deciding it.
//
//   sx = dx * src_w / dst_w
//   sy = dy * src_h / dst_h
//
// with truncating integer division in 64 bits, so the multiply cannot overflow
// for any dimensions a `u32` can express. There is no `+ 0.5` pixel-centre
// correction: the WIT says the filter is ugly on purpose.
//
// The three stated edges, in the WIT and so in both guests: a destination over
// 32 MiB is `malformed`; a zero-width or zero-height destination is a
// zero-pixel image, not an error; and sampling a zero-area source into a
// destination with area is `malformed`, because there is no pixel to copy --
// and because `dst_w / 0` traps.
bool Resize(Image* image, const watoots_asset_types_extent_t& extent,
            Failure* failure) {
  const auto src_w = static_cast<uint64_t>(image->width);
  const auto src_h = static_cast<uint64_t>(image->height);
  const auto dst_w = static_cast<uint64_t>(extent.width);
  const auto dst_h = static_cast<uint64_t>(extent.height);

  if (dst_w == 0 || dst_h == 0) {
    image->width = extent.width;
    image->height = extent.height;
    image->pixels.Reset(nullptr, 0);
    return true;
  }
  if (src_w == 0 || src_h == 0) {
    *failure =
        Malformed("cannot resize " + std::to_string(src_w) + "x" +
                  std::to_string(src_h) + " to " + std::to_string(dst_w) + "x" +
                  std::to_string(dst_h) + ": no source pixels to sample");
    return false;
  }

  const uint64_t bytes = dst_w * dst_h * kChannels;
  if (bytes > kMaxImageBytes) {
    *failure = Malformed("resize to " + std::to_string(dst_w) + "x" +
                         std::to_string(dst_h) + " would need " +
                         std::to_string(bytes) + " bytes, over the " +
                         std::to_string(kMaxImageBytes) + "-byte ceiling");
    return false;
  }

  auto* out = static_cast<uint8_t*>(std::malloc(static_cast<size_t>(bytes)));
  if (out == nullptr) {
    // The one place this guest can answer where the Rust one cannot. `bytes` is
    // under the WIT's ceiling, so getting here means the manifest's memory
    // limit is below it; Rust's `Vec::with_capacity` aborts, which reaches the
    // host as a trap. Saying so is strictly better and unreachable under any
    // manifest whose limit clears 32 MiB, which the shipped one does.
    *failure = Malformed("resize to " + std::to_string(dst_w) + "x" +
                         std::to_string(dst_h) + " would need " +
                         std::to_string(bytes) + " bytes, which could not be " +
                         "allocated under this manifest's memory limit");
    return false;
  }

  const uint8_t* src = image->pixels.Data();
  size_t at = 0;
  for (uint64_t dy = 0; dy < dst_h; ++dy) {
    const uint64_t sy = dy * src_h / dst_h;
    const auto row = static_cast<size_t>(sy * src_w * kChannels);
    for (uint64_t dx = 0; dx < dst_w; ++dx) {
      const uint64_t sx = dx * src_w / dst_w;
      std::memcpy(out + at, src + row + static_cast<size_t>(sx * kChannels), 3);
      at += 3;
    }
  }

  image->pixels.Reset(out, static_cast<size_t>(bytes));
  image->width = extent.width;
  image->height = extent.height;
  return true;
}

// -------------------------------------------------------------------------
// The lookup table
// -------------------------------------------------------------------------

// Rust's `str::lines`, reproduced: split on '\n', drop one trailing '\r', and
// do not yield a final empty line for a string that ends in a newline. The
// entry numbering in every parse error depends on this agreeing.
std::vector<std::string_view> Lines(std::string_view source) {
  std::vector<std::string_view> lines;
  size_t start = 0;
  while (start <= source.size()) {
    const size_t end = source.find('\n', start);
    if (end == std::string_view::npos) {
      if (start < source.size()) {
        lines.push_back(source.substr(start));
      }
      break;
    }
    std::string_view line = source.substr(start, end - start);
    if (!line.empty() && line.back() == '\r') {
      line.remove_suffix(1);
    }
    lines.push_back(line);
    start = end + 1;
  }
  return lines;
}

// Rust's `u8::is_ascii_whitespace`: space, tab, newline, form feed, carriage
// return. Not `std::isspace`, which also counts the vertical tab -- a one-byte
// difference that would make one guest skip a line the other tried to parse.
bool IsAsciiSpace(char byte) {
  return byte == ' ' || byte == '\t' || byte == '\n' || byte == '\x0c' ||
         byte == '\r';
}

// `str::trim`, near enough. Rust trims Unicode `White_Space`; this trims the
// ASCII set. The two differ only for a line whose leading run contains
// something like U+00A0, which no lookup table has, and the difference is
// recorded here rather than discovered later.
std::string_view Trim(std::string_view text) {
  while (!text.empty() && IsAsciiSpace(text.front())) {
    text.remove_prefix(1);
  }
  while (!text.empty() && IsAsciiSpace(text.back())) {
    text.remove_suffix(1);
  }
  return text;
}

std::vector<std::string_view> SplitAsciiWhitespace(std::string_view text) {
  std::vector<std::string_view> fields;
  size_t at = 0;
  while (at < text.size()) {
    while (at < text.size() && IsAsciiSpace(text[at])) {
      ++at;
    }
    const size_t start = at;
    while (at < text.size() && !IsAsciiSpace(text[at])) {
      ++at;
    }
    if (at > start) {
      fields.push_back(text.substr(start, at - start));
    }
  }
  return fields;
}

// `u8::from_str`, reproduced: an optional leading '+', then at least one ASCII
// digit, and nothing else; the value must fit in 8 bits. Leading zeros are
// fine, a '-' is not, and the range check *is* the parse -- which is why the
// error message can say "not three integers in 0..=255" for both a stray
// character and a 256.
bool ParseByte(std::string_view field, uint8_t* out) {
  size_t at = 0;
  if (at < field.size() && field[at] == '+') {
    ++at;
  }
  if (at == field.size()) {
    return false;
  }
  uint32_t value = 0;
  for (; at < field.size(); ++at) {
    if (field[at] < '0' || field[at] > '9') {
      return false;
    }
    value = value * 10 + static_cast<uint32_t>(field[at] - '0');
    if (value > 255) {
      return false;
    }
  }
  *out = static_cast<uint8_t>(value);
  return true;
}

struct Entry {
  uint8_t red = 0;
  uint8_t green = 0;
  uint8_t blue = 0;
};

bool ParseEntry(std::string_view line, Entry* out) {
  const std::vector<std::string_view> fields = SplitAsciiWhitespace(line);
  if (fields.size() != 3) {
    return false;
  }
  return ParseByte(fields[0], &out->red) && ParseByte(fields[1], &out->green) &&
         ParseByte(fields[2], &out->blue);
}

// Rust's `{:?}` for a `&str`, for the ASCII a lookup table can contain.
//
// The parse error quotes the offending line, and the two guests have to quote
// it the same way. Rust wraps the string in double quotes and escapes `\`, `"`,
// and the C escapes, rendering anything else unprintable as `\u{h}` with
// lowercase hex and no padding. Bytes above 0x7f are passed through, which
// matches Rust for printable scalars and differs for the grapheme-extended ones
// it also escapes -- reproducing that needs a Unicode table, and a table this
// plugin would consult once for a file that already failed to parse.
std::string EscapeDebug(std::string_view text) {
  std::string out = "\"";
  for (const char raw : text) {
    const auto byte = static_cast<unsigned char>(raw);
    switch (byte) {
      case '\t':
        out += "\\t";
        break;
      case '\r':
        out += "\\r";
        break;
      case '\n':
        out += "\\n";
        break;
      case '\\':
        out += "\\\\";
        break;
      case '"':
        out += "\\\"";
        break;
      default:
        if (byte < 0x20 || byte == 0x7f) {
          std::array<char, 8> hex{};
          std::snprintf(hex.data(), hex.size(), "\\u{%x}", byte);
          out += hex.data();
        } else {
          out += raw;
        }
        break;
    }
  }
  out += "\"";
  return out;
}

// Whether the bytes are valid UTF-8.
//
// Rust reads the table with `read_to_string`, which fails a file that is not
// UTF-8 with "stream did not contain valid UTF-8". C would happily parse the
// ASCII out of a binary file and report a different reason -- or none. This is
// the check that keeps the two answers the same.
bool IsUtf8(std::string_view text) {
  size_t at = 0;
  while (at < text.size()) {
    const auto lead = static_cast<unsigned char>(text[at]);
    size_t extra = 0;
    uint32_t code = 0;
    if (lead < 0x80) {
      at += 1;
      continue;
    }
    if ((lead & 0xe0) == 0xc0) {
      extra = 1;
      code = lead & 0x1fU;
    } else if ((lead & 0xf0) == 0xe0) {
      extra = 2;
      code = lead & 0x0fU;
    } else if ((lead & 0xf8) == 0xf0) {
      extra = 3;
      code = lead & 0x07U;
    } else {
      return false;
    }
    if (at + extra >= text.size()) {
      return false;
    }
    for (size_t k = 1; k <= extra; ++k) {
      const auto cont = static_cast<unsigned char>(text[at + k]);
      if ((cont & 0xc0) != 0x80) {
        return false;
      }
      code = (code << 6U) | (cont & 0x3fU);
    }
    // Overlong forms, surrogates and anything past U+10FFFF are all invalid.
    if ((extra == 1 && code < 0x80) || (extra == 2 && code < 0x800) ||
        (extra == 3 && code < 0x10000) || code > 0x10ffff ||
        (code >= 0xd800 && code <= 0xdfff)) {
      return false;
    }
    at += extra + 1;
  }
  return true;
}

// Turn a failed open or read into a `file-failure.reason`.
//
// `unreadable`'s doc names three causes -- not found, not permitted, not a
// lookup table -- and a WASI guest can only tell the third one apart. A path
// outside every preopened directory is reported as *not found*, identically to
// a missing file inside a granted one: the sandbox does not leak the existence
// of what it is not showing you. So this does not guess; it says what it saw
// and names the thing that is usually actually wrong.
//
// The text matches Rust's `io::Error` Display for a raw OS error, which is
// `strerror` followed by " (os error N)" -- the same libc string, because both
// guests are linked against the same wasi-libc.
std::string DescribeOpenFailure(int code) {
  const std::string message = std::string(std::strerror(code)) + " (os error " +
                              std::to_string(code) + ")";
  if (code == ENOENT) {
    return "cannot open it: " + message +
           " (a path outside every granted directory reports as not found, so "
           "check the manifest and the spelling of the path)";
  }
  return "cannot open it: " + message;
}

// Read a file, refusing to read more than `kMaxLutBytes` of it.
//
// The cap is the Rust guest's `Read::take`: a file that hits it is truncated
// and then fails the 256-entry check, which is the right answer for a file that
// large in any case.
bool ReadCapped(const std::string& path, std::string* text,
                std::string* reason) {
  std::FILE* file = std::fopen(path.c_str(), "rb");
  if (file == nullptr) {
    *reason = DescribeOpenFailure(errno);
    return false;
  }
  text->assign(kMaxLutBytes, '\0');
  const size_t got = std::fread(text->data(), 1, kMaxLutBytes, file);
  const bool failed = std::ferror(file) != 0;
  const int code = errno;
  std::fclose(file);
  if (failed) {
    *reason = DescribeOpenFailure(code);
    return false;
  }
  text->resize(got);
  if (!IsUtf8(*text)) {
    // Rust's `read_to_string` reports exactly this, and it is not an OS error,
    // so it does not go through `DescribeOpenFailure`.
    *reason = "cannot open it: stream did not contain valid UTF-8";
    return false;
  }
  return true;
}

// Read and parse a LUT file. The format is the Rust guest's, documented on its
// `load_lut` and demonstrated by `luts/sepia.lut`: 256 entries, one per line,
// each three decimal integers in 0..=255 in red green blue order; blank lines
// and `#` comments ignored; anything else must parse.
bool LoadLut(const std::string& path, std::vector<Entry>* table,
             std::string* reason) {
  std::string text;
  if (!ReadCapped(path, &text, reason)) {
    return false;
  }

  table->clear();
  table->reserve(256);
  const std::vector<std::string_view> lines = Lines(text);
  for (size_t index = 0; index < lines.size(); ++index) {
    const std::string_view line = Trim(lines[index]);
    if (line.empty() || line.front() == '#') {
      continue;
    }
    if (table->size() == 256) {
      *reason = "more than 256 entries: a 257th appears on line " +
                std::to_string(index + 1);
      return false;
    }
    Entry entry;
    if (!ParseEntry(line, &entry)) {
      *reason = "line " + std::to_string(index + 1) +
                " is not three integers in 0..=255: " + EscapeDebug(line);
      return false;
    }
    table->push_back(entry);
  }

  if (table->size() != 256) {
    *reason = "expected 256 entries, found " + std::to_string(table->size());
    return false;
  }
  return true;
}

// Apply a colour lookup table read from a file.
//
// The argument is a path, spelled the way the grant spells it. That is the
// WIT's rule, not this plugin's convention: a guest has no way to resolve a
// bare name -- this world does not import `wasi:filesystem/preopens` -- and
// watoots preopens each granted directory under the same string it was granted
// as. wasi-libc does the preopen matching inside `fopen`, so this opens the
// string as given and nothing more.
bool Lut(Image* image, std::string_view path, Failure* failure) {
  const std::string owned(path);
  std::vector<Entry> table;
  std::string reason;
  if (!LoadLut(owned, &table, &reason)) {
    *failure = Unreadable(path, std::move(reason));
    return false;
  }

  uint8_t* pixels = image->pixels.Data();
  for (size_t at = 0; at + 3 <= image->pixels.Size(); at += 3) {
    // Per channel, never across channels: red is looked up in the red column of
    // the entry the red input selects.
    pixels[at] = table[pixels[at]].red;
    pixels[at + 1] = table[pixels[at + 1]].green;
    pixels[at + 2] = table[pixels[at + 2]].blue;
  }
  return true;
}

// -------------------------------------------------------------------------

// Check that an image is an image before touching a byte of it.
//
// A pixel buffer whose length disagrees with the dimensions is exactly the
// input that turns an indexing bug into a trap. Untrusted input arrives here
// too, and in this language the consequence of not checking is worse.
bool Validate(const Image& image, Failure* failure) {
  const uint64_t expected = static_cast<uint64_t>(image.width) *
                            static_cast<uint64_t>(image.height) * kChannels;
  const auto actual = static_cast<uint64_t>(image.pixels.Size());
  if (actual != expected) {
    *failure = Malformed(std::to_string(image.width) + "x" +
                         std::to_string(image.height) + " is " +
                         std::to_string(expected) + " bytes of RGB8, got " +
                         std::to_string(actual));
    return false;
  }
  return true;
}

bool RunStep(Image* image, const watoots_asset_types_operation_t& step,
             Failure* failure) {
  switch (step.tag) {
    case WATOOTS_ASSET_TYPES_OPERATION_GRAYSCALE:
      Grayscale(image);
      return true;
    case WATOOTS_ASSET_TYPES_OPERATION_INVERT:
      Invert(image);
      return true;
    case WATOOTS_ASSET_TYPES_OPERATION_GAIN:
      ApplyGain(image, step.val.gain);
      return true;
    case WATOOTS_ASSET_TYPES_OPERATION_RESIZE:
      return Resize(image, step.val.resize, failure);
    default:
      return Lut(image, Borrow(step.val.lut), failure);
  }
}

}  // namespace

void exports_asset_plugin_describe(asset_plugin_plugin_info_t* ret) {
  ret->name = Own("cpp-asset");

  // `supports` is `list<operation-kind>`, so naming a step no longer means
  // constructing one. `malloc` because `cabi_post_describe` frees it.
  static constexpr watoots_asset_types_operation_kind_t kKinds[] = {
      WATOOTS_ASSET_TYPES_OPERATION_KIND_GRAYSCALE,
      WATOOTS_ASSET_TYPES_OPERATION_KIND_INVERT,
      WATOOTS_ASSET_TYPES_OPERATION_KIND_GAIN,
      WATOOTS_ASSET_TYPES_OPERATION_KIND_RESIZE,
      WATOOTS_ASSET_TYPES_OPERATION_KIND_LUT,
  };
  ret->supports.len = 0;
  ret->supports.ptr = static_cast<watoots_asset_types_operation_kind_t*>(
      std::malloc(sizeof(kKinds)));
  if (ret->supports.ptr != nullptr) {
    std::memcpy(ret->supports.ptr, static_cast<const void*>(kKinds),
                sizeof(kKinds));
    ret->supports.len = sizeof(kKinds) / sizeof(kKinds[0]);
  }
}

bool exports_asset_plugin_apply(asset_plugin_image_t* input,
                                asset_plugin_list_operation_t* steps,
                                asset_plugin_image_t* ret,
                                asset_plugin_failure_t* err) {
  // Calling into the host: an import crossing, and one of the things the
  // recorder captures. Deterministic on purpose -- no counts that depend on
  // anything but the arguments.
  Emit(WATOOTS_ASSET_LOG_LEVEL_INFO, "apply: " + std::to_string(input->width) +
                                         "x" + std::to_string(input->height) +
                                         ", " + std::to_string(steps->len) +
                                         " step(s)");

  Image image;
  image.width = input->width;
  image.height = input->height;
  // Parameters belong to the callee, so the pixel buffer is *taken* rather than
  // copied -- the same move the Rust guest gets from `input` being owned. The
  // input's own view is cleared so the free below cannot double-free it.
  image.pixels.Reset(input->pixels.ptr, input->pixels.len);
  input->pixels.ptr = nullptr;
  input->pixels.len = 0;

  Failure failure;
  bool ok = Validate(image, &failure);
  if (ok) {
    // "Steps apply in order. The whole call fails on the first step that cannot
    // be done" -- so a plain loop that stops, and a partly transformed image
    // never escapes.
    for (size_t at = 0; at < steps->len && ok; ++at) {
      ok = RunStep(&image, steps->ptr[at], &failure);
    }
  }

  // Nothing else frees these. Without it a plugin called in a loop grows the
  // guest's heap until the manifest's memory limit stops it -- measurably: the
  // C++ lint guest peaked at 19.7 MiB over 5000 calls without the frees and
  // 448 KiB with them.
  asset_plugin_image_free(input);
  asset_plugin_list_operation_free(steps);

  if (!ok) {
    Report(failure);
    *err = Lower(failure);
    return false;
  }

  ret->width = image.width;
  ret->height = image.height;
  ret->pixels.len = image.pixels.Size();
  ret->pixels.ptr = image.pixels.Release();
  return true;
}
