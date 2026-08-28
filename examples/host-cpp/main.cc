// A minimal C++ host application.
//
//   ./host_cpp <plugin.wasm> [file-to-lint]
//
// This is the artifact behind the claim that C++ applications have no
// component-model plugin option today. It loads a WebAssembly component,
// serves it a host function, calls typed exports, and never links Rust
// directly -- only watoots.hpp over the C API.

#include <cstdlib>
#include <fstream>
#include <iostream>
#include <iterator>
#include <span>
#include <string>
#include <vector>

#include "watoots.hpp"

namespace {

// Everything this plugin may touch. `clocks` and `env` are granted because a
// Rust guest built for wasm32-wasip2 imports them through std whether or not
// the plugin's author uses them.
constexpr const char* kPolicy = R"(
[permissions]
clocks = "monotonic"
env    = {}

[limits]
memory  = "64MiB"
fuel    = 200_000_000
timeout = "5s"
)";

int Fail(const wt::Error& error) {
  std::cerr << "watoots: " << wt_status_name(error.Code()) << ": "
            << error.Message() << '\n';
  return EXIT_FAILURE;
}

std::vector<std::byte> ReadFile(const std::string& path) {
  std::ifstream input(path, std::ios::binary);
  if (!input) {
    return {};
  }
  const std::vector<char> raw((std::istreambuf_iterator<char>(input)),
                              std::istreambuf_iterator<char>());
  std::vector<std::byte> bytes(raw.size());
  for (size_t index = 0; index < raw.size(); ++index) {
    bytes[index] = static_cast<std::byte>(raw[index]);
  }
  return bytes;
}

// Escape a string for WAVE. Values cross the boundary as text, so the host has
// to quote what it sends the same way it would for any other textual protocol.
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

}  // namespace

int main(int argc, char** argv) {
  const std::vector<std::string> args(argv, argv + argc);
  if (args.size() < 2) {
    std::cerr << "usage: " << args[0] << " <plugin.wasm> [file-to-lint]\n";
    return EXIT_FAILURE;
  }
  const std::string& plugin_path = args[1];

  std::cout << "watoots " << wt_version_string() << '\n';

  wt::HostBuilder builder;
  if (auto applied = builder.ManifestFromString(kPolicy); !applied) {
    return Fail(applied.error());
  }

  // The one capability this application offers plugins. The lambda captures,
  // which is why host functions are std::function rather than raw pointers.
  int log_lines = 0;
  auto served = builder.HostFunc(
      "watoots:example/log@0.1.0", "emit",
      [&log_lines](std::span<const std::string_view> emit_args)
          -> wt::Result<wt::Value> {
        if (emit_args.size() != 2) {
          return wt::unexpected(
              wt::Error(WT_ERR_INVALID_ARGUMENT, "emit takes two arguments"));
        }
        ++log_lines;
        std::cout << "  [plugin " << emit_args[0] << "] " << emit_args[1]
                  << '\n';
        return wt::Value{};  // emit returns nothing
      });
  if (!served) {
    return Fail(served.error());
  }

  auto host = builder.Build();
  if (!host) {
    return Fail(host.error());
  }

  // Show what the plugin would be granted, before running any of it.
  const std::vector<std::byte> wasm = ReadFile(plugin_path);
  if (wasm.empty()) {
    std::cerr << "cannot read " << plugin_path << '\n';
    return EXIT_FAILURE;
  }
  if (auto report = host->Inspect(wasm); report) {
    std::cout << "\ngrants for " << plugin_path << ":\n" << *report;
  } else {
    return Fail(report.error());
  }

  auto plugin = host->Load(plugin_path);
  if (!plugin) {
    return Fail(plugin.error());
  }
  std::cout << "\nloaded plugin: " << plugin->Name() << '\n';

  auto name = plugin->Call("name");
  if (!name) {
    return Fail(name.error());
  }
  std::cout << "name() -> " << name->value_or("(nothing)") << '\n';

  const std::string source =
      args.size() > 2 ? std::string(reinterpret_cast<const char*>(  // NOLINT
                                        ReadFile(args[2]).data()),
                                    ReadFile(args[2]).size())
                      : "fine line\nTODO: write a real host\ntrailing   \n";

  const std::vector<std::string> lint_args = {
      WaveString(args.size() > 2 ? args[2] : "example.txt"),
      WaveString(source)};

  std::cout << "\nlint():\n";
  auto diagnostics = plugin->Call("lint", lint_args);
  if (!diagnostics) {
    return Fail(diagnostics.error());
  }
  std::cout << "  " << diagnostics->value_or("(nothing)") << '\n';
  std::cout << "\nhost saw " << log_lines << " log line(s) from the plugin\n";

  return EXIT_SUCCESS;
}
