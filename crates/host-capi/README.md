# watoots-capi

The C API for [watoots](https://crates.io/crates/watoots), a sandboxed plugin
host for native applications on the WebAssembly component model. Built as a
static and a shared library, with a committed C header (`include/watoots.h`)
and a C++20 RAII header over it (`include/watoots.hpp`), so a C or C++
application links a prebuilt library and never needs a Rust toolchain.

```cpp
#include "watoots.hpp"

wt::HostBuilder builder;
builder.ManifestFromFile("policy.toml");
auto host = builder.Build();
auto plugin = host->Load("decoder.wasm");

std::vector<wt::Val> args;
args.push_back(wt::Val::Bytes(file_bytes));
auto out = plugin->Call("decode", args);      // typed: bytes stay bytes
auto text = plugin->Call("format");           // or WAVE text, for small values
```

```cmake
find_package(watoots REQUIRED)
target_link_libraries(my_app PRIVATE watoots::capi)
```

Two call paths over one call: `wt_plugin_call` takes and returns WAVE text,
which is right for a linter's arguments and wrong for a codec's, and
`wt_plugin_call_vals` carries typed `wt_val_t` values with no text in between.
Both go through the same limits, trace and audit, so a recording made through
one replays through the other. Every handle is opaque, every failure is a
status code plus a message, and no exception crosses the boundary.

Prefix `wt_`; every `wt_*_new` has a `wt_*_delete`. The CMake package is
installed from the repository's top-level `CMakeLists.txt`, which builds this
crate with cargo. The repository's
[README](https://github.com/vchance/watoots#readme) has a C++ previewer whose
format decoders are untrusted plugins, which is the example to read.

Licensed under Apache-2.0 WITH LLVM-exception.
