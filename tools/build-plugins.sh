#!/usr/bin/env bash
# Build the sample plugins.
#
#   tools/build-plugins.sh [rust|rust-asset|cpp|cpp-asset|js|py]...
#
# With no arguments, builds every guest whose toolchain is available and skips
# the rest with a note. Each needs a different toolchain, which is the point:
# the host does not care which one produced the component.
set -euo pipefail
cd "$(dirname "$0")/.."
root=$PWD

want() {
  [ "${#targets[@]}" -eq 0 ] && return 0
  for requested in "${targets[@]}"; do
    [ "$requested" = "$1" ] && return 0
  done
  return 1
}

targets=("$@")

if want rust; then
  if rustup target list --installed 2>/dev/null | grep -q wasm32-wasip2; then
    echo "==> rust-lint"
    cargo build --manifest-path examples/plugins/rust-lint/Cargo.toml \
      --target wasm32-wasip2 --release
    cp examples/plugins/rust-lint/target/wasm32-wasip2/release/rust_lint.wasm \
      examples/plugins/rust-lint/rust_lint.wasm
  else
    echo "skip rust-lint: rustup target add wasm32-wasip2"
  fi
fi

if want rust-asset; then
  # A second Rust guest, and the only sample that needs a capability: `lut`
  # opens its lookup table itself. Its own target, not folded into `rust`,
  # because the two prove different things and CI names the one it wants.
  if rustup target list --installed 2>/dev/null | grep -q wasm32-wasip2; then
    echo "==> rust-asset"
    cargo build --manifest-path examples/plugins/rust-asset/Cargo.toml \
      --target wasm32-wasip2 --release
    cp examples/plugins/rust-asset/target/wasm32-wasip2/release/rust_asset.wasm \
      examples/plugins/rust-asset/rust_asset.wasm
  else
    echo "skip rust-asset: rustup target add wasm32-wasip2"
  fi
fi

if want js; then
  if command -v npm >/dev/null 2>&1; then
    echo "==> js-lint"
    (cd examples/plugins/js-lint && npm install --silent && npm run --silent build)
  else
    echo "skip js-lint: npm not found"
  fi
fi

if want py; then
  if command -v python3 >/dev/null 2>&1; then
    echo "==> py-lint"
    cd "$root/examples/plugins/py-lint"
    [ -d .venv ] || python3 -m venv .venv
    ./.venv/bin/pip install --quiet componentize-py
    ./.venv/bin/componentize-py -d ../../wit/lint -w lint-plugin \
      componentize app -o py_lint.wasm
    cd "$root"
  else
    echo "skip py-lint: python3 not found"
  fi
fi

if want cpp || want cpp-asset; then
  # wasi-sdk is a 172MB tarball with no Homebrew formula, so it is found rather
  # than required: set WASI_SDK_PATH, or drop it in one of the usual places.
  # Found once and shared, because both C++ guests need the same two tools and
  # a second copy of this search is a second thing to keep in step.
  sdk=${WASI_SDK_PATH:-}
  if [ -z "$sdk" ]; then
    for candidate in "$HOME"/.local/share/wasi-sdk-* /opt/wasi-sdk "$HOME"/wasi-sdk; do
      [ -x "$candidate/bin/wasm32-wasip2-clang++" ] && sdk=$candidate && break
    done
  fi
  cpp_ready=no
  cpp_skip="no wasi-sdk (set WASI_SDK_PATH; see CONTRIBUTING.md)"
  if [ -n "$sdk" ] && [ -x "$sdk/bin/wasm32-wasip2-clang++" ]; then
    if command -v wit-bindgen >/dev/null 2>&1; then
      cpp_ready=yes
    else
      cpp_skip="wit-bindgen not found (cargo install wit-bindgen-cli)"
    fi
  fi
fi

if want cpp; then
  if [ "$cpp_ready" = yes ]; then
    echo "==> cpp-lint"
    cd "$root/examples/plugins/cpp-lint"
    # Regenerated every build: the bindings are a function of the WIT, so a
    # stale copy is a silent disagreement with the world everyone else compiled.
    wit-bindgen c ../../wit/lint --world lint-plugin --out-dir bindings >/dev/null
    # The generated bindings are C. Compiling them with clang++ mangles the
    # component-type force-link symbol and the link fails on a name nothing
    # explains, so they get their own C compile.
    "$sdk/bin/wasm32-wasip2-clang" -std=c11 -O2 -I. -c bindings/lint_plugin.c -o bindings.o
    "$sdk/bin/wasm32-wasip2-clang++" -std=c++20 -O2 -fno-exceptions -fno-rtti -I. \
      -o cpp_lint.wasm lint.cc bindings.o bindings/lint_plugin_component_type.o
    cd "$root"
  else
    echo "skip cpp-lint: $cpp_skip"
  fi
fi

if want cpp-asset; then
  # The second C++ guest, and the one that has to agree with rust-asset on
  # every output byte. Its own target for the same reason `rust-asset` is not
  # folded into `rust`: the two prove different things.
  if [ "$cpp_ready" = yes ]; then
    echo "==> cpp-asset"
    cd "$root/examples/plugins/cpp-asset"
    wit-bindgen c ../../wit/asset --world asset-plugin --out-dir bindings >/dev/null
    # C bindings with clang, C++ with clang++ -- see the note above cpp-lint.
    "$sdk/bin/wasm32-wasip2-clang" -std=c11 -O2 -I. -c bindings/asset_plugin.c -o bindings.o
    # `-ffp-contract=off` on top of cpp-lint's flags. `gain` is a multiply and
    # an add, which is exactly the shape a compiler is allowed to fuse; wasm has
    # no fma instruction so it cannot happen here, and the flag says so out loud
    # rather than relying on the target to keep the guests in agreement.
    "$sdk/bin/wasm32-wasip2-clang++" -std=c++20 -O2 -fno-exceptions -fno-rtti \
      -ffp-contract=off -I. \
      -o cpp_asset.wasm asset.cc bindings.o bindings/asset_plugin_component_type.o
    cd "$root"
  else
    echo "skip cpp-asset: $cpp_skip"
  fi
fi

echo
echo "built components:"
ls -1 examples/plugins/*/*.wasm 2>/dev/null || echo "  (none)"
