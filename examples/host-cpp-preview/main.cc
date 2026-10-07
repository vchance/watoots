// A file previewer whose format decoders are plugins.
//
//   ./host_cpp_preview <policy.toml> <in.file> <out.png> <decoder.wasm>...
//
// This is the shape of every desktop application's most dangerous feature:
// "open a file", where the file came from the internet and the code that
// parses it came from a third party. Finder's Quick Look plugins and Explorer's
// shell extensions are the famous versions. Here the decoders are components,
// each loaded under a policy that grants nothing the codec asked for, and the
// host never sees a file format -- it sees bytes go in and pixels come out.
//
// What a reader should watch for:
//
// - `sniff` before `decode`. Each installed decoder is asked, from the first
//   few bytes only, whether this looks like its format. That is the dispatch,
//   and a decoder answers it without allocating.
// - The pixel count is checked before it is trusted. `image.pixels` is
//   documented as `width * height * 4` bytes, and a decoder that returns fewer
//   is refused rather than believed -- an untrusted plugin must not be able to
//   make a host read past a buffer.
// - `examples/fixtures/preview/bomb.qoi`. A valid file claiming a 1 GiB image.
//   The decoder asks for the memory, the sandbox refuses, and this host gets
//   `WT_ERR_LIMIT_EXCEEDED` naming `limits.memory` -- not a crash, not an OOM
//   kill, and not "wasm trap: unreachable". It prints one line and exits
//   cleanly. That line is the reason this example exists.
//
// - Bytes cross as bytes. The file goes in as `wt::Val::Bytes` and the pixels
//   come back through `AsBytes()`: the typed call path, not the WAVE text one
//   the lint host uses. A linter's arguments are a few words; a codec's are a
//   file, and a file rendered as `[113, 111, 105, ...]` is four characters per
//   byte on both sides of the boundary.
//
// The audit hook is installed and forwards ceiling events to stderr, because a
// viewer that can say "the QOI codec hit its memory limit on this file" is a
// viewer whose user knows what happened.

#include <algorithm>
#include <cstddef>
#include <cstdint>
#include <cstdlib>
#include <fstream>
#include <iostream>
#include <iterator>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

#include "watoots.hpp"

#include "stb_image_write.h"

namespace {

// How much of the file `sniff` is shown. Every real magic number fits in far
// less; the point of a small prefix is that a decoder cannot do real work on
// it.
constexpr size_t kSniffBytes = 16;

// First line only. A watoots message leads with the cause and puts the guest
// backtrace underneath; a viewer's user wants the cause, and a developer who
// wants the frames has `watoots run` for that.
int Fail(std::string_view what, const wt::Error& error) {
  const std::string& message = error.Message();
  const std::string_view headline(message.data(),
                                  std::min(message.find('\n'), message.size()));
  std::cerr << "preview: " << what << ": " << wt_status_name(error.Code())
            << ": " << headline << '\n';
  return EXIT_FAILURE;
}

// ---------------------------------------------------------------------------
// The decoder's answers
// ---------------------------------------------------------------------------

struct Image {
  uint32_t width = 0;
  uint32_t height = 0;
  std::vector<unsigned char> rgba;
};

// One of the three `failure` cases, already rendered for a human.
struct Failure {
  bool not_this_format = false;
  std::string description;
};

wt::Error Malformed(std::string what) {
  return {WT_ERR_INVALID_ARGUMENT,
          "the decoder's answer is not an image: " + std::move(what)};
}

// `result<image, failure>`, walked rather than parsed. Every accessor answers
// `nullopt` for a value of the wrong kind, and this host treats each one as a
// refusal: the plugin's answer has to be the shape its world declares before a
// byte of it is used.
wt::Result<std::optional<Image>> ReadDecodeResult(wt::ValRef answer,
                                                  Failure& failure) {
  const std::optional<bool> ok = answer.IsOk();
  if (!ok.has_value()) {
    return wt::unexpected(Malformed("not a result"));
  }

  if (!*ok) {
    const std::optional<wt::ValRef> why = answer.Payload();
    const std::optional<std::string_view> which =
        why ? why->Case() : std::nullopt;
    if (!which.has_value()) {
      return wt::unexpected(Malformed("an err with no failure case"));
    }
    if (*which == "not-this-format") {
      failure.not_this_format = true;
    } else if (*which == "truncated") {
      const std::optional<wt::ValRef> missing = why->Payload();
      const std::optional<uint64_t> count =
          missing ? missing->AsU64() : std::nullopt;
      failure.description = "truncated: about " +
                            std::to_string(count.value_or(0)) + " more needed";
    } else if (*which == "corrupt") {
      const std::optional<wt::ValRef> text = why->Payload();
      const std::optional<std::string_view> reason =
          text ? text->AsString() : std::nullopt;
      failure.description = "corrupt: " + std::string(reason.value_or(""));
    } else {
      return wt::unexpected(
          Malformed("unknown failure case " + std::string(*which)));
    }
    return std::optional<Image>{};
  }

  const std::optional<wt::ValRef> record = answer.Payload();
  if (!record.has_value()) {
    return wt::unexpected(Malformed("an ok with no image"));
  }
  const std::optional<wt::ValRef> width = record->Field("width");
  const std::optional<wt::ValRef> height = record->Field("height");
  const std::optional<wt::ValRef> pixels = record->Field("pixels");
  const std::optional<uint64_t> w = width ? width->AsU64() : std::nullopt;
  const std::optional<uint64_t> h = height ? height->AsU64() : std::nullopt;
  if (!w || !h || !pixels) {
    return wt::unexpected(Malformed("missing width, height or pixels"));
  }
  Image image;
  image.width = static_cast<uint32_t>(*w);
  image.height = static_cast<uint32_t>(*h);

  // The check that matters. The dimensions are the decoder's claim; the byte
  // count is what it actually handed over, and they have to agree before this
  // host reads a single pixel. `AsBytes` is one copy out of the value, which
  // is the whole of what the pixels cost on this side.
  std::optional<std::vector<uint8_t>> rgba = pixels->AsBytes();
  if (!rgba.has_value()) {
    return wt::unexpected(Malformed("pixels is not a list<u8>"));
  }
  const uint64_t expected =
      static_cast<uint64_t>(image.width) * image.height * 4;
  if (rgba->size() != expected) {
    return wt::unexpected(Malformed(
        "the decoder returned " + std::to_string(rgba->size()) +
        " pixel bytes for a " + std::to_string(image.width) + "x" +
        std::to_string(image.height) + " image; refusing to trust it"));
  }
  image.rgba.assign(rgba->begin(), rgba->end());
  return std::optional<Image>{std::move(image)};
}

// ---------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------

std::optional<std::vector<uint8_t>> ReadFile(const std::string& path) {
  std::ifstream in(path, std::ios::binary);
  if (!in) {
    return std::nullopt;
  }
  return std::vector<uint8_t>(std::istreambuf_iterator<char>(in), {});
}

bool WritePng(const std::string& path, const Image& image) {
  return stbi_write_png(path.c_str(), static_cast<int>(image.width),
                        static_cast<int>(image.height), 4, image.rgba.data(),
                        static_cast<int>(image.width) * 4) != 0;
}

// ---------------------------------------------------------------------------
// The host
// ---------------------------------------------------------------------------

struct Decoder {
  wt::Plugin plugin;
  std::string format;
};

void Usage(const std::string& program) {
  std::cerr << "usage: " << program
            << " <policy.toml> <in.file> <out.png> <decoder.wasm>...\n";
}

}  // namespace

int main(int argc, char** argv) {
  const std::vector<std::string> args(argv, argv + argc);
  if (args.size() < 5) {
    Usage(args[0]);
    return EXIT_FAILURE;
  }
  const std::string& policy = args[1];
  const std::string& input_path = args[2];
  const std::string& output_path = args[3];
  const std::span<const std::string> decoder_paths(args.begin() + 4,
                                                   args.end());

  wt::HostBuilder builder;
  if (auto applied = builder.ManifestFromFile(policy); !applied) {
    return Fail("policy", applied.error());
  }
  // Forward ceiling events. `kind` is checked rather than the line grepped:
  // a C++ host gets the structured fields precisely so it does not have to.
  if (auto hooked = builder.AuditHook([](const wt::AuditEvent& event) {
        if (event.kind == WT_AUDIT_CEILING_SPENT) {
          std::cerr << "preview: " << event.plugin << " hit " << event.limit_key
                    << '\n';
        }
      });
      !hooked) {
    return Fail("audit hook", hooked.error());
  }
  auto host = builder.Build();
  if (!host) {
    return Fail("host", host.error());
  }

  // Install the decoders. Each is loaded under the same policy, which is the
  // realistic shape: one "codec plugins may do this much" rule, not one per
  // codec. A decoder that wants more than that is refused here, before it has
  // seen a byte of anyone's file.
  std::vector<Decoder> decoders;
  for (const std::string& path : decoder_paths) {
    auto plugin = host->Load(path);
    if (!plugin) {
      return Fail(path, plugin.error());
    }
    auto format = plugin->Call("format", std::span<const wt::Val>{});
    if (!format) {
      return Fail(path + ": format", format.error());
    }
    // The borrowed string_view lives as long as the `Val` it was read from,
    // which is the `format` result; copy it out before that goes away.
    const std::optional<std::string_view> name =
        format->has_value() ? (*format)->AsString() : std::nullopt;
    if (!name.has_value()) {
      return Fail(path + ": format", Malformed("not a string"));
    }
    decoders.push_back({std::move(*plugin), std::string(*name)});
  }

  auto bytes = ReadFile(input_path);
  if (!bytes) {
    std::cerr << "preview: cannot read " << input_path << '\n';
    return EXIT_FAILURE;
  }

  // Dispatch on the magic bytes. Each decoder sees at most kSniffBytes and
  // answers yes or no; the first yes gets the whole file.
  const std::span<const uint8_t> prefix(bytes->data(),
                                        std::min(bytes->size(), kSniffBytes));
  std::vector<wt::Val> sniff_args;
  sniff_args.push_back(wt::Val::Bytes(prefix));

  for (Decoder& decoder : decoders) {
    auto sniffed = decoder.plugin.Call("sniff", sniff_args);
    if (!sniffed) {
      return Fail(decoder.format + ": sniff", sniffed.error());
    }
    const std::optional<bool> yes =
        sniffed->has_value() ? (*sniffed)->AsBool() : std::nullopt;
    if (!yes.value_or(false)) {
      continue;
    }

    std::cerr << "preview: " << input_path << " looks like " << decoder.format
              << "; decoding\n";
    // The whole file, copied once into a `list<u8>`. This is the argument the
    // text path could not carry well, and the reason this host is typed.
    std::vector<wt::Val> decode_args;
    decode_args.push_back(wt::Val::Bytes(std::span<const uint8_t>(*bytes)));
    auto decoded = decoder.plugin.Call("decode", decode_args);
    if (!decoded) {
      // This is where the bomb lands. The sandbox refused the allocation, the
      // decoder could not continue, and the error says which ceiling -- so a
      // user sees "this file claims more than a codec is allowed", which is
      // the truth, rather than a crash, which is what they would have seen
      // with the codec in-process.
      return Fail(decoder.format + ": decode", decoded.error());
    }

    if (!decoded->has_value()) {
      return Fail(decoder.format + ": answer", Malformed("no value"));
    }
    Failure failure;
    auto image = ReadDecodeResult(**decoded, failure);
    if (!image) {
      return Fail(decoder.format + ": answer", image.error());
    }
    if (!*image) {
      if (failure.not_this_format) {
        // It sniffed yes and then changed its mind. Legitimate -- a prefix is
        // not a proof -- so try the next one.
        std::cerr << "preview: " << decoder.format
                  << " declined on a closer look\n";
        continue;
      }
      std::cerr << "preview: " << decoder.format << ": " << failure.description
                << '\n';
      return EXIT_FAILURE;
    }

    if (!WritePng(output_path, **image)) {
      std::cerr << "preview: cannot write " << output_path << '\n';
      return EXIT_FAILURE;
    }
    std::cout << "wrote " << output_path << ": " << (*image)->width << "x"
              << (*image)->height << " RGBA8 via " << decoder.format << '\n';
    return EXIT_SUCCESS;
  }

  std::cerr << "preview: no installed decoder recognises " << input_path << " ("
            << decoders.size() << " tried)\n";
  return EXIT_FAILURE;
}
