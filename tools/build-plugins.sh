#!/usr/bin/env bash
# Build the three sample plugins.
#
#   tools/build-plugins.sh [rust|js|py]...
#
# With no arguments, builds every guest whose toolchain is available and skips
# the rest with a note. Each needs a different toolchain, which is the point:
# the host does not care which one produced the component.
set -euo pipefail
cd "$(dirname "$0")/.."
root=$PWD

want() {
  [ "$#" -eq 0 ] && return 0
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
    ./.venv/bin/componentize-py -d ../../wit -w lint-plugin \
      componentize app -o py_lint.wasm
    cd "$root"
  else
    echo "skip py-lint: python3 not found"
  fi
fi

echo
echo "built components:"
ls -1 examples/plugins/*/*.wasm 2>/dev/null || echo "  (none)"
