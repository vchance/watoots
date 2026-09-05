#!/usr/bin/env bash
# Build the sample plugins.
#
#   tools/build-plugins.sh [rust|rust-asset|cpp|js|py]...
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

if want cpp; then
  # wasi-sdk is a 172MB tarball with no Homebrew formula, so it is found rather
  # than required: set WASI_SDK_PATH, or drop it in one of the usual places.
  sdk=${WASI_SDK_PATH:-}
  if [ -z "$sdk" ]; then
    for candidate in "$HOME"/.local/share/wasi-sdk-* /opt/wasi-sdk "$HOME"/wasi-sdk; do
      [ -x "$candidate/bin/wasm32-wasip2-clang++" ] && sdk=$candidate && break
    done
  fi
  if [ -n "$sdk" ] && [ -x "$sdk/bin/wasm32-wasip2-clang++" ] &&
    command -v wit-bindgen >/dev/null 2>&1; then
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
  elif [ -z "$sdk" ]; then
    echo "skip cpp-lint: no wasi-sdk (set WASI_SDK_PATH; see CONTRIBUTING.md)"
  else
    echo "skip cpp-lint: wit-bindgen not found (cargo install wit-bindgen-cli)"
  fi
fi

echo
echo "built components:"
ls -1 examples/plugins/*/*.wasm 2>/dev/null || echo "  (none)"
