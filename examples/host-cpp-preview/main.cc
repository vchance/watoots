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
#include "wave_reader.hpp"

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
// The decoder's answers, read out of WAVE
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

// `ok({width: W, height: H, pixels: [..]})` or `err(<failure>)`.
wt::Result<std::optional<Image>> ReadDecodeResult(std::string_view text,
                                                  Failure& failure) {
  wave::WaveReader reader(text);

  if (reader.Take('e')) {
    reader.Expect("rr(");
    const std::string which = reader.Word();
    if (which == "not-this-format") {
      failure.not_this_format = true;
    } else if (which == "truncated") {
      reader.Expect('(');
      const uint64_t missing = reader.Number64();
      reader.Expect(')');
      failure.description =
          "truncated: about " + std::to_string(missing) + " more needed";
    } else if (which == "corrupt") {
      reader.Expect('(');
      failure.description = "corrupt: " + reader.Text();
      reader.Expect(')');
    } else {
      reader.Reject("unknown failure case " + which);
    }
    reader.Expect(')');
    if (!reader.Ok()) {
      return wt::unexpected(reader.Failure());
    }
    return std::optional<Image>{};
  }

  reader.Expect("ok({");
  Image image;
  reader.Expect("width:");
  image.width = reader.Number();
  reader.Expect(',');
  reader.Expect("height:");
  image.height = reader.Number();
  reader.Expect(',');
  reader.Expect("pixels:");
  reader.Expect('[');

  // The check that matters. The dimensions are the decoder's claim; the byte
  // count is what it actually handed over, and they have to agree before this
  // host reads a single pixel.
  const uint64_t expected =
      static_cast<uint64_t>(image.width) * image.height * 4;
  image.rgba.reserve(static_cast<size_t>(expected));
  while (reader.Ok() && !reader.Peek(']')) {
    const uint32_t byte = reader.Number();
    if (byte > 255) {
      reader.Reject("a pixel byte over 255");
      break;
    }
    if (image.rgba.size() == expected) {
      reader.Reject("more pixel bytes than width * height * 4");
      break;
    }
    image.rgba.push_back(static_cast<unsigned char>(byte));
    reader.Take(',');
  }
  reader.Expect(']');
  reader.Expect("})");
  if (!reader.Ok()) {
    return wt::unexpected(reader.Failure());
  }
  if (image.rgba.size() != expected) {
    return wt::unexpected(wave::Unreadable(
        "the decoder returned " + std::to_string(image.rgba.size()) +
        " pixel bytes for a " + std::to_string(image.width) + "x" +
        std::to_string(image.height) + " image; refusing to trust it"));
  }
  return std::optional<Image>{std::move(image)};
}

// ---------------------------------------------------------------------------
// Writing WAVE
// ---------------------------------------------------------------------------

// A `list<u8>` as text. This is the honest cost of the C API's text-only call
// path, and examples/README.md says what it costs; for a previewer it is fine,
// and for a video decoder it would not be.
std::string WaveBytes(std::span<const unsigned char> bytes) {
  std::string out;
  out.reserve(bytes.size() * 4 + 2);
  out.push_back('[');
  for (size_t i = 0; i < bytes.size(); ++i) {
    if (i != 0) {
      out.push_back(',');
    }
    out += std::to_string(bytes[i]);
  }
  out.push_back(']');
  return out;
}

// ---------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------

std::optional<std::vector<unsigned char>> ReadFile(const std::string& path) {
  std::ifstream in(path, std::ios::binary);
  if (!in) {
    return std::nullopt;
  }
  return std::vector<unsigned char>(std::istreambuf_iterator<char>(in), {});
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
    auto format = plugin->Call("format");
    if (!format) {
      return Fail(path + ": format", format.error());
    }
    // Named, not a temporary: `WaveReader` borrows a `string_view`, so the
    // answer has to outlive the reader. `value_or` returns by value, and a
    // reader built straight from it reads a dead stack slot -- which worked in
    // the plain build and was a stack-use-after-scope under AddressSanitizer,
    // the first time this host ran under it.
    const std::string format_text = format->value_or("");
    wave::WaveReader reader(format_text);
    std::string name = reader.Text();
    if (!reader.Ok()) {
      return Fail(path + ": format", reader.Failure());
    }
    decoders.push_back({std::move(*plugin), std::move(name)});
  }

  auto bytes = ReadFile(input_path);
  if (!bytes) {
    std::cerr << "preview: cannot read " << input_path << '\n';
    return EXIT_FAILURE;
  }

  // Dispatch on the magic bytes. Each decoder sees at most kSniffBytes and
  // answers yes or no; the first yes gets the whole file.
  const std::span<const unsigned char> prefix(
      bytes->data(), std::min(bytes->size(), kSniffBytes));
  const std::vector<std::string> sniff_args{WaveBytes(prefix)};

  for (Decoder& decoder : decoders) {
    auto sniffed = decoder.plugin.Call("sniff", sniff_args);
    if (!sniffed) {
      return Fail(decoder.format + ": sniff", sniffed.error());
    }
    if (sniffed->value_or("") != "true") {
      continue;
    }

    std::cerr << "preview: " << input_path << " looks like " << decoder.format
              << "; decoding\n";
    const std::vector<std::string> decode_args{WaveBytes(*bytes)};
    auto decoded = decoder.plugin.Call("decode", decode_args);
    if (!decoded) {
      // This is where the bomb lands. The sandbox refused the allocation, the
      // decoder could not continue, and the error says which ceiling -- so a
      // user sees "this file claims more than a codec is allowed", which is
      // the truth, rather than a crash, which is what they would have seen
      // with the codec in-process.
      return Fail(decoder.format + ": decode", decoded.error());
    }

    Failure failure;
    // Same rule. The reader inside `ReadDecodeResult` only lives for the call,
    // so a temporary would be safe today; naming it costs nothing and removes
    // the refactor that would make it unsafe tomorrow.
    const std::string decoded_text = decoded->value_or("");
    auto image = ReadDecodeResult(decoded_text, failure);
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
