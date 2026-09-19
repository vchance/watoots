// A QOI decoder as a watoots plugin, in C++.
//
//   tools/build-plugins.sh cpp-qoi
//
// The same world as the Rust decoder, byte for byte on the same inputs; the
// host is not recompiled between them. C++ as the *untrusted* side is the half
// of this project's claim that a C++ host alone does not demonstrate.
//
// Like the Rust decoder, this checks the arithmetic and nothing else about
// size. It cannot know the host's memory budget, so it asks, and under
// `limits.memory` the sandbox refuses. What happens then is worth a sentence,
// because wasm makes it different from native: address 0 is valid linear
// memory, so a NULL from `malloc` that goes unchecked does not fault -- the
// decoder would write its pixels from address zero, over its own data, until
// it ran off the end of memory. `abort()` on NULL is what a correct program
// does anyway, it is what Rust's allocator does, and it is what turns the
// refusal into a clean `LimitExceeded` rather than a corrupted heap.

#include <cstdint>
#include <cstdlib>
#include <cstring>
#include <string_view>

extern "C" {
#include "bindings/decoder.h"
}

namespace {

constexpr std::string_view kMagic = "qoif";
constexpr size_t kHeaderLen = 14;
constexpr uint8_t kEndMarker[8] = {0, 0, 0, 0, 0, 0, 0, 1};

// Copy into a WIT string. `cabi_post_*` frees export results with `free`, so
// this has to come from `malloc` -- see cpp-lint for the four ownership rules.
decoder_string_t Own(std::string_view text) {
  decoder_string_t out{nullptr, 0};
  if (text.empty()) return out;
  auto* buffer = static_cast<uint8_t*>(std::malloc(text.size()));
  if (buffer == nullptr) return out;
  std::memcpy(buffer, text.data(), text.size());
  out.ptr = buffer;
  out.len = text.size();
  return out;
}

decoder_failure_t NotThisFormat() {
  decoder_failure_t f{};
  f.tag = WATOOTS_PREVIEW_TYPES_FAILURE_NOT_THIS_FORMAT;
  return f;
}

decoder_failure_t Truncated(uint64_t missing) {
  decoder_failure_t f{};
  f.tag = WATOOTS_PREVIEW_TYPES_FAILURE_TRUNCATED;
  f.val.truncated = missing;
  return f;
}

decoder_failure_t Corrupt(std::string_view why) {
  decoder_failure_t f{};
  f.tag = WATOOTS_PREVIEW_TYPES_FAILURE_CORRUPT;
  f.val.corrupt = Own(why);
  return f;
}

struct Pixel {
  uint8_t r, g, b, a;
};

size_t Hash(Pixel p) { return (p.r * 3 + p.g * 5 + p.b * 7 + p.a * 11) % 64; }

uint32_t ReadU32(const uint8_t* at) {
  return (static_cast<uint32_t>(at[0]) << 24) |
         (static_cast<uint32_t>(at[1]) << 16) |
         (static_cast<uint32_t>(at[2]) << 8) | static_cast<uint32_t>(at[3]);
}

// The decoder proper, over a borrowed view. Returns true and fills `ret`, or
// false and fills `err`.
bool Decode(const uint8_t* data, size_t len, decoder_image_t* ret,
            decoder_failure_t* err) {
  if (len < kMagic.size() ||
      std::memcmp(data, kMagic.data(), kMagic.size()) != 0) {
    *err = NotThisFormat();
    return false;
  }
  if (len < kHeaderLen) {
    *err = Truncated(kHeaderLen - len);
    return false;
  }

  const uint32_t width = ReadU32(data + 4);
  const uint32_t height = ReadU32(data + 8);
  const uint8_t channels = data[12];
  if (channels < 3 || channels > 4) {
    *err = Corrupt("channels must be 3 or 4");
    return false;
  }
  if (width == 0 || height == 0) {
    *err = Corrupt("zero dimension");
    return false;
  }

  // Overflow is a correctness question and gets checked. Whether the size is
  // affordable is a policy question, and is not.
  const uint64_t pixel_count = static_cast<uint64_t>(width) * height;
  const uint64_t byte_count = pixel_count * 4;
  if (byte_count / 4 != pixel_count || byte_count > SIZE_MAX) {
    *err = Corrupt("dimensions overflow");
    return false;
  }

  // The allocation a hostile header aims at. Under `limits.memory` the
  // sandbox refuses it, malloc returns NULL, and abort() is the honest answer.
  auto* pixels =
      static_cast<uint8_t*>(std::malloc(static_cast<size_t>(byte_count)));
  if (pixels == nullptr) std::abort();

  Pixel index[64] = {};
  Pixel px{0, 0, 0, 255};
  size_t pos = kHeaderLen;
  const size_t body_end =
      len >= sizeof(kEndMarker) ? len - sizeof(kEndMarker) : 0;
  size_t written = 0;
  const size_t total = static_cast<size_t>(byte_count);

  auto emit = [&](Pixel p) {
    pixels[written++] = p.r;
    pixels[written++] = p.g;
    pixels[written++] = p.b;
    pixels[written++] = p.a;
  };

  while (written < total) {
    if (pos >= body_end) {
      std::free(pixels);
      *err = Truncated((total - written) / 4);
      return false;
    }
    const uint8_t b1 = data[pos++];

    if (b1 == 0xFE) {  // QOI_OP_RGB
      if (pos + 3 > body_end) {
        std::free(pixels);
        *err = Truncated(pos + 3 - body_end);
        return false;
      }
      px.r = data[pos];
      px.g = data[pos + 1];
      px.b = data[pos + 2];
      pos += 3;
    } else if (b1 == 0xFF) {  // QOI_OP_RGBA
      if (pos + 4 > body_end) {
        std::free(pixels);
        *err = Truncated(pos + 4 - body_end);
        return false;
      }
      px = {data[pos], data[pos + 1], data[pos + 2], data[pos + 3]};
      pos += 4;
    } else {
      switch (b1 >> 6) {
        case 0:  // QOI_OP_INDEX
          px = index[b1 & 0x3F];
          break;
        case 1:  // QOI_OP_DIFF: three 2-bit deltas, bias 2
          px.r += static_cast<uint8_t>(((b1 >> 4) & 0x03) - 2);
          px.g += static_cast<uint8_t>(((b1 >> 2) & 0x03) - 2);
          px.b += static_cast<uint8_t>((b1 & 0x03) - 2);
          break;
        case 2: {  // QOI_OP_LUMA: 6-bit green delta, then 4+4 red/blue vs green
          if (pos >= body_end) {
            std::free(pixels);
            *err = Truncated(1);
            return false;
          }
          const uint8_t b2 = data[pos++];
          const auto dg = static_cast<uint8_t>((b1 & 0x3F) - 32);
          const auto dr_dg = static_cast<uint8_t>((b2 >> 4) - 8);
          const auto db_dg = static_cast<uint8_t>((b2 & 0x0F) - 8);
          px.r += static_cast<uint8_t>(dg + dr_dg);
          px.g += dg;
          px.b += static_cast<uint8_t>(dg + db_dg);
          break;
        }
        default: {  // QOI_OP_RUN: repeat previous pixel, bias 1
          const size_t run = (b1 & 0x3F) + 1;
          for (size_t i = 0; i < run && written < total; ++i) emit(px);
          index[Hash(px)] = px;
          continue;
        }
      }
    }

    index[Hash(px)] = px;
    emit(px);
  }

  if (len < pos + sizeof(kEndMarker) ||
      std::memcmp(data + len - sizeof(kEndMarker), kEndMarker,
                  sizeof(kEndMarker)) != 0) {
    std::free(pixels);
    *err = Corrupt("missing end marker");
    return false;
  }

  ret->width = width;
  ret->height = height;
  ret->pixels.ptr = pixels;
  ret->pixels.len = total;
  return true;
}

}  // namespace

void exports_decoder_format(decoder_string_t* ret) { *ret = Own("qoi"); }

bool exports_decoder_sniff(decoder_list_u8_t* prefix) {
  const bool yes = prefix->len >= kMagic.size() &&
                   std::memcmp(prefix->ptr, kMagic.data(), kMagic.size()) == 0;
  // Export parameters belong to the callee.
  decoder_list_u8_free(prefix);
  return yes;
}

bool exports_decoder_decode(decoder_list_u8_t* data, decoder_image_t* ret,
                            decoder_failure_t* err) {
  const bool ok = Decode(data->ptr, data->len, ret, err);
  // Export parameters belong to the callee; nothing else frees this.
  decoder_list_u8_free(data);
  return ok;
}
