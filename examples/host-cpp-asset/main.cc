// A C++ host application for the asset world.
//
//   ./host_cpp_asset <plugin.wasm> <policy.toml> <in.png> <out.png> <steps>
//
// The sibling of examples/host-cpp, and deliberately a second binary rather
// than a second mode on that one: they demonstrate different things, and a
// host that switched worlds on argv would demonstrate neither clearly.
//
// What this one is for. `asset.wit` names no file format anywhere, because the
// *host* owns the codec: it decodes PNG to RGB8 here, hands over pixels, and
// encodes what comes back. That is how a real pipeline is built -- decode once
// at the edge -- and it is also the only way four guest languages agree byte
// for byte without four PNG dependencies. An untrusted plugin never parses a
// file format.
//
// It also calls `describe` before it sends anything and refuses a step the
// plugin does not advertise. That is what `plugin-info` is for: routing a
// pipeline without trial and error.
//
// The honest caveat, and half of why this example was written: images cross
// the C API as WAVE *text*, because `wt_plugin_call` takes `const char* const*`
// and there is no binary path. It works. It is not free. examples/README.md
// says what it costs, and the profile printed at the end of every run says the
// same thing from the other side.

#include <algorithm>
#include <array>
#include <chrono>
#include <cstddef>
#include <cstdint>
#include <cstdlib>
#include <exception>
#include <iomanip>
#include <iostream>
#include <memory>
#include <optional>
#include <span>
#include <sstream>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

#include "watoots.hpp"

#include "stb_image.h"
#include "stb_image_write.h"

namespace {

// The one capability this application offers, spelled as the components import
// it. Registering it with `HostFunc` is also what declares it to the grant
// check, which is why no policy in examples/policies grants it.
constexpr const char* kLogInterface = "watoots:asset/log@0.1.0";

// RGB8, forced on load. `image.pixels` is documented as `width * height * 3`
// bytes, so a greyscale or RGBA PNG is converted at the edge rather than
// becoming a contract the guest cannot rely on.
constexpr int kChannels = 3;

wt::Error Invalid(std::string message) {
  return {WT_ERR_INVALID_ARGUMENT, std::move(message)};
}

// A value the plugin returned that this host cannot read. Deliberately not
// WT_ERR_INVALID_ARGUMENT: the argument was ours, the answer is theirs, and
// conflating the two sends the reader to the wrong file.
wt::Error Unreadable(const std::string& message) {
  return {WT_ERR_INTERNAL, "the plugin's answer did not parse: " + message};
}

int Fail(const wt::Error& error) {
  std::cerr << "watoots: " << wt_status_name(error.Code()) << ": "
            << error.Message() << '\n';
  return EXIT_FAILURE;
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

// ---------------------------------------------------------------------------
// The world's values
// ---------------------------------------------------------------------------

struct Image {
  uint32_t width = 0;
  uint32_t height = 0;
  std::vector<unsigned char> pixels;
};

// `[10, 20, 30, ...]`, reserving `expected` bytes up front: growing a
// multi-megabyte vector one push at a time is measurable at these sizes.
std::vector<unsigned char> ReadPixels(WaveReader& reader, uint64_t expected) {
  std::vector<unsigned char> pixels;
  pixels.reserve(static_cast<size_t>(expected));
  reader.Expect('[');
  while (reader.Ok() && !reader.Peek(']')) {
    pixels.push_back(static_cast<unsigned char>(reader.Number()));
    if (!reader.Take(',')) {
      break;
    }
  }
  reader.Expect(']');
  return pixels;
}

// `{width: .., height: .., pixels: [..]}`.
Image ReadImage(WaveReader& reader) {
  Image image;
  reader.Expect('{');
  while (reader.Ok() && !reader.Peek('}')) {
    const std::string field = reader.Word();
    reader.Expect(':');
    if (field == "width") {
      image.width = reader.Number();
    } else if (field == "height") {
      image.height = reader.Number();
    } else if (field == "pixels") {
      // WAVE prints a record's fields in declaration order, so the dimensions
      // are known by the time the pixels arrive.
      const uint64_t expected =
          static_cast<uint64_t>(image.width) * image.height * kChannels;
      image.pixels = ReadPixels(reader, expected);
    } else {
      reader.SkipValue();
    }
    if (!reader.Take(',')) {
      break;
    }
  }
  reader.Expect('}');

  // The check `asset.wit` asks the host to make, and the reason it asks: a
  // length that disagrees with the dimensions is exactly the kind of thing an
  // untrusted plugin should not be able to make a host act on. stb would read
  // off the end of the buffer.
  const uint64_t expected =
      static_cast<uint64_t>(image.width) * image.height * kChannels;
  if (reader.Ok() && image.pixels.size() != expected) {
    reader.Reject("the plugin returned " + std::to_string(image.pixels.size()) +
                  " byte(s) of pixels for a " + std::to_string(image.width) +
                  "x" + std::to_string(image.height) + " image, which needs " +
                  std::to_string(expected));
  }
  return image;
}

struct PluginInfo {
  std::string name;
  std::vector<std::string> supports;
};

// `{name: "...", supports: [grayscale, invert, ...]}`.
PluginInfo ReadPluginInfo(WaveReader& reader) {
  PluginInfo info;
  reader.Expect('{');
  while (reader.Ok() && !reader.Peek('}')) {
    const std::string field = reader.Word();
    reader.Expect(':');
    if (field == "name") {
      info.name = reader.Text();
    } else if (field == "supports") {
      reader.Expect('[');
      while (reader.Ok() && !reader.Peek(']')) {
        info.supports.push_back(reader.Word());
        if (!reader.Take(',')) {
          break;
        }
      }
      reader.Expect(']');
    } else {
      reader.SkipValue();
    }
    if (!reader.Take(',')) {
      break;
    }
  }
  reader.Expect('}');
  return info;
}

// `{path: "...", reason: "..."}`, the payload of `unreadable`.
std::string ReadFileFailure(WaveReader& reader) {
  std::string path;
  std::string reason;
  reader.Expect('{');
  while (reader.Ok() && !reader.Peek('}')) {
    const std::string field = reader.Word();
    reader.Expect(':');
    if (field == "path") {
      path = reader.Text();
    } else if (field == "reason") {
      reason = reader.Text();
    } else {
      reader.SkipValue();
    }
    if (!reader.Take(',')) {
      break;
    }
  }
  reader.Expect('}');
  return "the plugin could not read " + path + ": " + reason;
}

// One `failure`, rendered for a person.
//
// Branching on the case and never on the prose, because `asset.wit` is explicit
// that the strings are for people and that two guests wording the same failure
// differently are both conforming.
std::string ReadFailure(WaveReader& reader) {
  const std::string which = reader.Word();
  reader.Expect('(');
  std::string described;
  if (which == "unsupported") {
    // Only reachable for a step `describe` advertised, since anything else is
    // refused above before a pixel is sent -- so this is the plugin
    // contradicting itself, and worth saying so.
    described = "the plugin declined `" + reader.Word() +
                "`, which its own describe() advertised";
  } else if (which == "malformed") {
    described = "the plugin rejected the image: " + reader.Text();
  } else if (which == "unreadable") {
    described = ReadFileFailure(reader);
  } else {
    // A case this host has not been taught. Naming it beats guessing at its
    // payload, and `asset.wit` warns that `operation` and `operation-kind` are
    // kept in step by hand.
    reader.SkipValue();
    described =
        "the plugin returned a failure case this host does not know: `" +
        which + "`";
  }
  reader.Expect(')');
  return described;
}

// ---------------------------------------------------------------------------
// Writing WAVE
// ---------------------------------------------------------------------------

// "0, " through "255, ", built once.
//
// The loop below runs once per byte of the image -- three million times for a
// 1024x1024 photograph -- and formatting a number inside it is most of what the
// encode costs. A table turns each iteration into one short append.
const std::array<std::string, 256>& Decimals() {
  static const std::array<std::string, 256> kTable = [] {
    std::array<std::string, 256> table;
    for (size_t value = 0; value < table.size(); ++value) {
      table.at(value) = std::to_string(value) + ", ";
    }
    return table;
  }();
  return kTable;
}

// Escape a string for WAVE, as examples/host-cpp does: values cross the
// boundary as text, so the host quotes what it sends.
std::string WaveString(std::string_view text) {
  std::string out = "\"";
  for (const char character : text) {
    switch (character) {
      case '"':
        out += "\\\"";
        break;
      case '\\':
        out += "\\\\";
        break;
      case '\n':
        out += "\\n";
        break;
      case '\t':
        out += "\\t";
        break;
      default:
        out += character;
    }
  }
  out += '"';
  return out;
}

// The image, as the WAVE text `wt_plugin_call` takes.
//
// This function is the cost examples/README.md quotes. One byte of pixel
// becomes up to five characters of argument, so the reserve is not a
// micro-optimisation: without it this reallocates and copies a growing
// multi-megabyte string dozens of times.
std::string WaveImage(const Image& image) {
  constexpr size_t kWorstCaseCharsPerByte = 5;  // "255, "
  std::string out;
  out.reserve((image.pixels.size() * kWorstCaseCharsPerByte) + 64);
  out += "{width: ";
  out += std::to_string(image.width);
  out += ", height: ";
  out += std::to_string(image.height);
  out += ", pixels: [";
  const std::array<std::string, 256>& decimals = Decimals();
  for (const unsigned char value : image.pixels) {
    out += decimals.at(value);
  }
  if (!image.pixels.empty()) {
    out.resize(out.size() - 2);  // the trailing ", "
  }
  out += "]}";
  return out;
}

// ---------------------------------------------------------------------------
// The pipeline, as the command line spells it
// ---------------------------------------------------------------------------

struct Step {
  // The `operation-kind` this step is, which is what `describe` answers in.
  std::string kind;
  // The `operation` value, ready to send.
  std::string wave;
  // How the step is echoed back to the person who typed it.
  std::string label;
};

std::vector<std::string_view> Split(std::string_view text, char separator) {
  std::vector<std::string_view> parts;
  size_t start = 0;
  while (true) {
    const size_t found = text.find(separator, start);
    if (found == std::string_view::npos) {
      parts.push_back(text.substr(start));
      return parts;
    }
    parts.push_back(text.substr(start, found - start));
    start = found + 1;
  }
}

std::optional<uint32_t> ParseU32(std::string_view text) {
  if (text.empty()) {
    return std::nullopt;
  }
  uint64_t value = 0;
  for (const char character : text) {
    if (character < '0' || character > '9') {
      return std::nullopt;
    }
    value = (value * 10) + static_cast<uint64_t>(character - '0');
    if (value > UINT32_MAX) {
      return std::nullopt;
    }
  }
  return static_cast<uint32_t>(value);
}

std::optional<float> ParseFloat(std::string_view text) {
  std::istringstream input{std::string(text)};
  float value = 0;
  input >> value;
  if (input.fail() || !input.eof()) {
    return std::nullopt;
  }
  return value;
}

// f32 for WAVE: the shortest spelling that reads back as the same float.
//
// Nine significant digits always round-trip, but printing `1.2` as
// `1.20000005` in the echoed pipeline is a worse answer than trying six digits
// first and only widening when it does not survive the trip. The trailing
// ".0" keeps a whole number reading as a float rather than an integer.
std::string WaveFloat(float value) {
  constexpr int kShortest = 6;
  constexpr int kRoundTrips = 9;
  std::string text;
  for (int digits = kShortest; digits <= kRoundTrips; ++digits) {
    std::ostringstream out;
    out << std::setprecision(digits) << value;
    text = out.str();
    const std::optional<float> back = ParseFloat(text);
    if (back.has_value() && *back == value) {
      break;
    }
  }
  if (text.find_first_of(".eEni") == std::string::npos) {
    text += ".0";
  }
  return text;
}

wt::Result<Step> ParseGain(std::string_view arguments) {
  const std::vector<std::string_view> parts = Split(arguments, ':');
  if (parts.size() != 2) {
    return wt::unexpected(
        Invalid("gain wants `gain:<red|green|blue>:<factor>`"));
  }
  const std::string channel(parts.at(0));
  if (channel != "red" && channel != "green" && channel != "blue") {
    return wt::unexpected(
        Invalid("`" + channel + "` is not a channel; use red, green or blue"));
  }
  const std::optional<float> factor = ParseFloat(parts.at(1));
  if (!factor.has_value()) {
    return wt::unexpected(
        Invalid("`" + std::string(parts.at(1)) + "` is not a gain factor"));
  }
  // Not clamped here on purpose: `asset.wit` puts the 0.0..=4.0 clamp and the
  // NaN rule on the guest, and a host that clamped first would hide a guest
  // that does not.
  const std::string spelled = WaveFloat(*factor);
  return Step{
      .kind = "gain",
      .wave = "gain({channel: " + channel + ", factor: " + spelled + "})",
      .label = "gain(" + channel + " x" + spelled + ")",
  };
}

wt::Result<Step> ParseResize(std::string_view arguments) {
  const std::vector<std::string_view> parts = Split(arguments, 'x');
  if (parts.size() != 2) {
    return wt::unexpected(Invalid("resize wants `resize:<width>x<height>`"));
  }
  const std::optional<uint32_t> width = ParseU32(parts.at(0));
  const std::optional<uint32_t> height = ParseU32(parts.at(1));
  if (!width.has_value() || !height.has_value()) {
    return wt::unexpected(
        Invalid("`" + std::string(arguments) + "` is not a <width>x<height>"));
  }
  const std::string spelled =
      std::to_string(*width) + "x" + std::to_string(*height);
  return Step{
      .kind = "resize",
      .wave = "resize({width: " + std::to_string(*width) +
              ", height: " + std::to_string(*height) + "})",
      .label = "resize(" + spelled + ")",
  };
}

// One step of the comma-separated pipeline:
//
//   grayscale
//   invert
//   gain:<red|green|blue>:<factor>
//   resize:<width>x<height>
//   lut:<path>
wt::Result<Step> ParseStep(std::string_view spelling) {
  const size_t colon = spelling.find(':');
  const std::string kind(spelling.substr(0, colon));
  const std::string_view arguments = colon == std::string_view::npos
                                         ? std::string_view{}
                                         : spelling.substr(colon + 1);

  if (kind == "grayscale" || kind == "invert") {
    if (colon != std::string_view::npos) {
      return wt::unexpected(Invalid(kind + " takes no arguments"));
    }
    return Step{.kind = kind, .wave = kind, .label = kind};
  }
  // Before the missing-argument complaint, so a misspelled step is told it is
  // misspelled rather than told to give arguments to something that does not
  // exist.
  if (kind != "gain" && kind != "resize" && kind != "lut") {
    return wt::unexpected(
        Invalid("`" + kind +
                "` is not a step; use grayscale, invert, gain, resize or lut"));
  }
  if (colon == std::string_view::npos) {
    return wt::unexpected(
        Invalid("`" + kind + "` needs arguments, as `" + kind + ":...`"));
  }
  if (kind == "gain") {
    return ParseGain(arguments);
  }
  if (kind == "resize") {
    return ParseResize(arguments);
  }
  // `lut`, the only name the check above leaves. Everything after the first
  // colon, so a path with a colon in it survives. `asset.wit` is explicit that
  // this is a path spelled the way the manifest's grant spells it, not a name
  // the plugin can resolve.
  return Step{
      .kind = "lut",
      .wave = "lut(" + WaveString(arguments) + ")",
      .label = "lut(" + std::string(arguments) + ")",
  };
}

wt::Result<std::vector<Step>> ParsePipeline(std::string_view spelling) {
  std::vector<Step> steps;
  for (const std::string_view part : Split(spelling, ',')) {
    if (part.empty()) {
      return wt::unexpected(Invalid("an empty step in the pipeline"));
    }
    auto step = ParseStep(part);
    if (!step) {
      return wt::unexpected(step.error());
    }
    steps.push_back(*std::move(step));
  }
  return steps;
}

// ---------------------------------------------------------------------------
// PNG, which is the host's job and nobody else's
// ---------------------------------------------------------------------------

struct StbFree {
  void operator()(unsigned char* pixels) const noexcept {
    stbi_image_free(pixels);
  }
};

wt::Result<Image> ReadPng(const std::string& path) {
  int width = 0;
  int height = 0;
  int channels_in_file = 0;
  // The last argument forces three channels whatever the file holds, which is
  // what makes `image.pixels` mean the same thing for every input.
  const std::unique_ptr<unsigned char, StbFree> decoded(
      stbi_load(path.c_str(), &width, &height, &channels_in_file, kChannels));
  if (decoded == nullptr) {
    const char* reason = stbi_failure_reason();
    return wt::unexpected(Invalid("cannot decode " + path + ": " +
                                  (reason == nullptr ? "not a PNG" : reason)));
  }
  const size_t count = static_cast<size_t>(width) * height * kChannels;
  const std::span<const unsigned char> pixels(decoded.get(), count);
  return Image{
      .width = static_cast<uint32_t>(width),
      .height = static_cast<uint32_t>(height),
      .pixels = std::vector<unsigned char>(pixels.begin(), pixels.end()),
  };
}

wt::Result<void> WritePng(const std::string& path, const Image& image) {
  if (image.width == 0 || image.height == 0) {
    // A legal answer -- `asset.wit` says a zero-dimension `resize` destination
    // is a zero-pixel image and not an error -- and not a legal PNG. Saying so
    // beats writing a file no decoder will open.
    return wt::unexpected(
        Invalid("the pipeline produced a " + std::to_string(image.width) + "x" +
                std::to_string(image.height) +
                " image, which is a valid answer but not a PNG"));
  }
  const int stride = static_cast<int>(image.width) * kChannels;
  if (stbi_write_png(path.c_str(), static_cast<int>(image.width),
                     static_cast<int>(image.height), kChannels,
                     image.pixels.data(), stride) == 0) {
    return wt::unexpected(Invalid("cannot write " + path));
  }
  return {};
}

// ---------------------------------------------------------------------------
// Reporting
// ---------------------------------------------------------------------------

using Clock = std::chrono::steady_clock;

std::string Millis(Clock::duration elapsed) {
  const auto micros =
      std::chrono::duration_cast<std::chrono::microseconds>(elapsed).count();
  return std::to_string(micros / 1000) + "." +
         std::to_string((micros % 1000) / 100) + " ms";
}

std::string MillisFromNanos(uint64_t nanos) {
  return Millis(std::chrono::nanoseconds(nanos));
}

uint64_t Percent(uint64_t part, uint64_t whole) {
  return whole == 0 ? 0 : (part * 100) / whole;
}

// Where the plugin's time went, as watoots measured it at the boundary.
//
// Printed on every run because for this workload the split *is* the finding:
// almost all of it lands in `marshalling`, the bucket that holds the canonical
// ABI's copying and watoots' own dispatch -- and, here, the WAVE text. It is a
// diagnostic and not an accounting identity; see ADR-0009.
void ReportProfile(const wt::PluginProfile& profile) {
  std::cout << "\nwhere the time went (watoots profile)\n"
            << "  wall         " << MillisFromNanos(profile.wall_nanos) << '\n'
            << "  guest        " << MillisFromNanos(profile.guest_nanos) << " ("
            << Percent(profile.guest_nanos, profile.wall_nanos) << "%)\n"
            << "  host calls   " << MillisFromNanos(profile.host_nanos) << " ("
            << Percent(profile.host_nanos, profile.wall_nanos) << "%)\n"
            << "  marshalling  " << MillisFromNanos(profile.marshalling_nanos)
            << " (" << Percent(profile.marshalling_nanos, profile.wall_nanos)
            << "%) -- the pixels, as WAVE text, in both directions\n";
}

void Usage(const std::string& program) {
  std::cerr << "usage: " << program
            << " <plugin.wasm> <policy.toml> <in.png> <out.png> "
               "<step>[,<step>...]\n"
               "\nsteps:\n"
               "  grayscale\n"
               "  invert\n"
               "  gain:<red|green|blue>:<factor>\n"
               "  resize:<width>x<height>\n"
               "  lut:<path>    spelled the way the policy's fs.read grant "
               "spells it\n";
}

// ---------------------------------------------------------------------------

// Build a host that serves `watoots:asset/log` and profiles the boundary.
wt::Result<wt::Host> OpenHost(const std::string& policy_path) {
  wt::HostBuilder builder;
  // Unlike examples/host-cpp there is no built-in fallback policy. The four
  // asset guests need four different ones -- compare rust-asset.toml with
  // py-asset.toml -- and a default that happened to fit one of them would
  // teach the wrong lesson.
  if (auto applied = builder.ManifestFromFile(policy_path); !applied) {
    return wt::unexpected(applied.error());
  }
  // The one capability this application offers. `wasi:filesystem`, which the
  // `lut` step needs, is not here: that is the manifest's business, granted
  // per plugin under ${plugin_dir}.
  auto served = builder.HostFunc(
      kLogInterface, "emit",
      [](std::span<const std::string_view> args) -> wt::Result<wt::Value> {
        if (args.size() != 2) {
          return wt::unexpected(
              wt::Error(WT_ERR_INVALID_ARGUMENT, "emit takes two arguments"));
        }
        // Both arguments arrive as WAVE, so the message is a quoted string.
        // Unquoting it is one more use of the reader, and the reason the
        // reader is not an implementation detail of the result path.
        WaveReader message(args[1]);
        const std::string prose = message.Text();
        std::cout << "  [plugin " << args[0] << "] "
                  << (message.Ok() ? prose : std::string(args[1])) << '\n';
        return wt::Value{};  // emit returns nothing
      });
  if (!served) {
    return wt::unexpected(served.error());
  }
  // Opt in to the boundary profile: for this workload the split is the point.
  if (auto profiled = builder.Profile(); !profiled) {
    return wt::unexpected(profiled.error());
  }
  return builder.Build();
}

int Run(const std::vector<std::string>& args) {
  constexpr size_t kExpectedArgs = 6;
  if (args.size() != kExpectedArgs) {
    Usage(args.empty() ? "host_cpp_asset" : args.at(0));
    return EXIT_FAILURE;
  }
  const std::string& plugin_path = args.at(1);
  const std::string& policy_path = args.at(2);
  const std::string& input_path = args.at(3);
  const std::string& output_path = args.at(4);
  const std::string& pipeline_spelling = args.at(5);

  // Parsed before anything is loaded: a typo in the pipeline should not cost a
  // component instantiation to discover.
  auto steps = ParsePipeline(pipeline_spelling);
  if (!steps) {
    return Fail(steps.error());
  }

  std::cout << "watoots " << wt_version_string() << '\n';
  auto host = OpenHost(policy_path);
  if (!host) {
    return Fail(host.error());
  }
  std::cout << "policy: " << policy_path << '\n';

  auto plugin = host->Load(plugin_path);
  if (!plugin) {
    return Fail(plugin.error());
  }
  std::cout << "loaded plugin: " << plugin->Name() << '\n';

  // `describe` first, and route on it. This is what `plugin-info` is for: the
  // pipeline is checked against what the plugin says it implements before a
  // single pixel is marshalled, so a step nobody can do costs a name rather
  // than a megabyte of WAVE and a failure.
  auto described = plugin->Call("describe");
  if (!described) {
    return Fail(described.error());
  }
  // Named, not a temporary: WaveReader borrows a string_view, so the answer
  // has to outlive it.
  const std::string describe_text = described->value_or("");
  WaveReader describe_reader(describe_text);
  const PluginInfo info = ReadPluginInfo(describe_reader);
  if (!describe_reader.Ok()) {
    return Fail(describe_reader.Failure());
  }
  std::cout << "describe() -> " << info.name << ", supports";
  for (const std::string& kind : info.supports) {
    std::cout << ' ' << kind;
  }
  std::cout << '\n';

  for (const Step& step : *steps) {
    if (std::ranges::find(info.supports, step.kind) == info.supports.end()) {
      std::cerr << "watoots: " << info.name << " does not implement `"
                << step.kind << "`, so `" << pipeline_spelling
                << "` cannot run against it\n";
      return EXIT_FAILURE;
    }
  }

  // The host owns the codec. Nothing past this line knows what a PNG is.
  auto image = ReadPng(input_path);
  if (!image) {
    return Fail(image.error());
  }
  std::cout << "read " << input_path << ": " << image->width << 'x'
            << image->height << " RGB8, " << image->pixels.size()
            << " bytes of pixels\n";

  std::string pipeline;
  std::string pipeline_wave = "[";
  for (const Step& step : *steps) {
    if (!pipeline.empty()) {
      pipeline += ", ";
      pipeline_wave += ", ";
    }
    pipeline += step.label;
    pipeline_wave += step.wave;
  }
  pipeline_wave += ']';
  std::cout << "pipeline: " << pipeline << '\n';

  const Clock::time_point encode_began = Clock::now();
  const std::vector<std::string> call_args = {WaveImage(*image), pipeline_wave};
  const Clock::duration encoded = Clock::now() - encode_began;
  std::cout << "\nWAVE argument: " << call_args.at(0).size()
            << " bytes of text for " << image->pixels.size()
            << " bytes of pixels, built in " << Millis(encoded) << '\n';

  std::cout << "apply():\n";
  const Clock::time_point call_began = Clock::now();
  auto answer = plugin->Call("apply", call_args);
  const Clock::duration called = Clock::now() - call_began;
  if (!answer) {
    return Fail(answer.error());
  }
  const std::string result = answer->value_or("");
  std::cout << "  answered with " << result.size() << " bytes of WAVE in "
            << Millis(called) << '\n';

  const Clock::time_point parse_began = Clock::now();
  WaveReader answer_reader(result);
  const std::string outcome = answer_reader.Word();
  answer_reader.Expect('(');
  if (outcome == "err") {
    // A failure the plugin *returned* is an answer, not a crash. The process
    // exits non-zero because the pipeline did not run, but nothing trapped and
    // the sandbox was never involved.
    const std::string described_failure = ReadFailure(answer_reader);
    if (!answer_reader.Ok()) {
      return Fail(answer_reader.Failure());
    }
    std::cerr << "watoots: apply failed: " << described_failure << '\n';
    return EXIT_FAILURE;
  }
  if (outcome != "ok") {
    return Fail(
        Unreadable("expected ok(...) or err(...), got `" + outcome + "`"));
  }
  const Image output = ReadImage(answer_reader);
  answer_reader.Expect(')');
  if (!answer_reader.Ok()) {
    return Fail(answer_reader.Failure());
  }
  const Clock::duration parsed = Clock::now() - parse_began;
  std::cout << "  parsed it in " << Millis(parsed) << '\n';

  if (auto written = WritePng(output_path, output); !written) {
    return Fail(written.error());
  }
  std::cout << "wrote " << output_path << ": " << output.width << 'x'
            << output.height << " RGB8\n";

  auto profile = plugin->Profile();
  if (!profile) {
    return Fail(profile.error());
  }
  ReportProfile(*profile);
  return EXIT_SUCCESS;
}

}  // namespace

int main(int argc, char** argv) {
  try {
    // argv is a counted array; this is the one place its count and its pointer
    // have to meet. Every other index in this file is bounds-checked.
    // NOLINTNEXTLINE(cppcoreguidelines-pro-bounds-pointer-arithmetic)
    const std::vector<std::string> args(argv, argv + argc);
    return Run(args);
  } catch (const std::exception& error) {
    // A host that lets an exception escape main gets std::terminate and no
    // diagnostic, which is a poor advertisement for a sandbox.
    std::cerr << "watoots: " << error.what() << '\n';
    return EXIT_FAILURE;
  }
}
