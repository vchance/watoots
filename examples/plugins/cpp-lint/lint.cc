// A sample watoots plugin, in C++.
//
//   tools/build-plugins.sh cpp
//
// Same world as the Rust, JavaScript and Python samples, same host, same
// diagnostics. This one exists because the project's claim is that C++
// applications have no component-model plugin option today — and that claim is
// only half demonstrated by a C++ *host*. Here C++ is the untrusted side.
//
// `wit-bindgen` generates C, not C++, which is the same shape as the host side:
// `watoots.hpp` is a C++ layer over a C API. The boundary is C either way, and
// the language on each side of it is a local choice.

#include <cstdlib>
#include <cstring>
#include <string>
#include <string_view>
#include <vector>

extern "C" {
#include "bindings/lint_plugin.h"
}

namespace {

// Copy into a WIT string.
//
// The canonical ABI frees everything this function returns — see
// `cabi_post_lint` in the generated shim, which calls `free` on the list and on
// each message. So the allocation has to come from `malloc`, and a `new[]` here
// would be a heap mismatch the host could never see coming.
lint_plugin_string_t Own(std::string_view text) {
  lint_plugin_string_t out{nullptr, 0};
  if (text.empty()) {
    // A zero-length malloc may return nullptr, which the ABI reads as an empty
    // string anyway. Being explicit costs nothing and removes the ambiguity.
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

void Emit(watoots_example_log_severity_t level, std::string_view message) {
  lint_plugin_string_t owned = Own(message);
  watoots_example_log_emit(level, &owned);
  // `emit` takes the string by value in WIT, so the host owns it now.
}

std::string_view Borrow(const lint_plugin_string_t& text) {
  return {reinterpret_cast<const char*>(text.ptr), text.len};
}

// Rust's `str::lines`, reproduced: split on '\n', drop one trailing '\r', and
// do not yield a final empty line for a string that ends in a newline. The
// three guests have to agree here or the same input lints differently
// depending on who compiled the plugin.
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

struct Finding {
  uint32_t line;
  uint32_t column;
  watoots_example_types_severity_t severity;
  std::string message;
};

}  // namespace

void exports_lint_plugin_name(lint_plugin_string_t* ret) {
  *ret = Own("cpp-lint");
}

void exports_lint_plugin_lint(lint_plugin_string_t* path,
                              lint_plugin_string_t* source,
                              lint_plugin_list_diagnostic_t* ret) {
  Emit(WATOOTS_EXAMPLE_TYPES_SEVERITY_HINT,
       "linting " + std::string(Borrow(*path)));

  std::vector<Finding> findings;
  const std::vector<std::string_view> lines = Lines(Borrow(*source));

  for (size_t index = 0; index < lines.size(); ++index) {
    const std::string_view line = lines[index];
    const auto number = static_cast<uint32_t>(index + 1);

    if (line.size() > 80) {
      findings.push_back(
          {number, 81, WATOOTS_EXAMPLE_TYPES_SEVERITY_WARNING,
           "line is " + std::to_string(line.size()) + " characters, over 80"});
    }

    if (!line.empty() && (line.back() == ' ' || line.back() == '\t')) {
      findings.push_back({number, static_cast<uint32_t>(line.size()),
                          WATOOTS_EXAMPLE_TYPES_SEVERITY_HINT,
                          "trailing whitespace"});
    }

    if (const size_t todo = line.find("TODO"); todo != std::string_view::npos) {
      findings.push_back({number, static_cast<uint32_t>(todo + 1),
                          WATOOTS_EXAMPLE_TYPES_SEVERITY_ERROR,
                          "unresolved TODO"});
    }
  }

  Emit(WATOOTS_EXAMPLE_TYPES_SEVERITY_HINT,
       std::to_string(findings.size()) + " diagnostic(s)");

  ret->len = findings.size();
  ret->ptr = nullptr;
  if (!findings.empty()) {
    // malloc, again because `cabi_post_lint` frees this with `free`.
    ret->ptr = static_cast<lint_plugin_diagnostic_t*>(
        std::malloc(findings.size() * sizeof(lint_plugin_diagnostic_t)));
    if (ret->ptr == nullptr) {
      ret->len = 0;
    } else {
      for (size_t i = 0; i < findings.size(); ++i) {
        ret->ptr[i].line = findings[i].line;
        ret->ptr[i].column = findings[i].column;
        ret->ptr[i].severity = findings[i].severity;
        ret->ptr[i].message = Own(findings[i].message);
      }
    }
  }

  // Parameters to an exported function belong to the callee. Nothing else
  // frees these, and a plugin called in a loop would grow the guest's heap
  // until the manifest's memory limit stopped it.
  lint_plugin_string_free(path);
  lint_plugin_string_free(source);
}
