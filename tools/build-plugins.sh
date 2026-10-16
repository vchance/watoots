#!/usr/bin/env bash
# Build the sample plugins.
#
#   tools/build-plugins.sh [<target>]...
#
# With no arguments, builds every guest whose toolchain is available and skips
# the rest with a note. Naming a target whose toolchain is missing is an
# error: a CI step that asks for `cpp-qoi` must not pass by building nothing.
# Each guest needs a different toolchain, which is the point: the host does not
# care which one produced the component.
#
# The targets are the rows of the table at the bottom; `--list` prints them.
set -euo pipefail
cd "$(dirname "$0")/.."
root=$PWD

# ---------------------------------------------------------------------------
# Toolchains, probed once
# ---------------------------------------------------------------------------

have_rust_target() {
  [ -n "${rust_ready:-}" ] && return "$rust_ready"
  if rustup target list --installed 2>/dev/null | grep -q wasm32-wasip2; then
    rust_ready=0
  else
    rust_ready=1
  fi
  return $rust_ready
}

have_npm() { command -v npm >/dev/null 2>&1; }
have_python() { command -v python3 >/dev/null 2>&1; }

# wasi-sdk is a 172MB tarball with no Homebrew formula, so it is found rather
# than required: set WASI_SDK_PATH, or drop it in one of the usual places.
# Found once and shared, because every C++ guest needs the same two tools and a
# second copy of this search is a second thing to keep in step.
have_wasi_sdk() {
  [ -n "${cpp_ready:-}" ] && return "$cpp_ready"
  cpp_ready=1
  cpp_skip="no wasi-sdk (set WASI_SDK_PATH; see CONTRIBUTING.md)"
  sdk=${WASI_SDK_PATH:-}
  if [ -z "$sdk" ]; then
    for candidate in "$HOME"/.local/share/wasi-sdk-* /opt/wasi-sdk "$HOME"/wasi-sdk; do
      [ -x "$candidate/bin/wasm32-wasip2-clang++" ] && sdk=$candidate && break
    done
  fi
  if [ -n "$sdk" ] && [ -x "$sdk/bin/wasm32-wasip2-clang++" ]; then
    if command -v wit-bindgen >/dev/null 2>&1; then
      cpp_ready=0
    else
      cpp_skip="wit-bindgen not found (cargo install wit-bindgen-cli)"
    fi
  fi
  return $cpp_ready
}

# ---------------------------------------------------------------------------
# One builder per toolchain
# ---------------------------------------------------------------------------

# A Rust guest: `cargo build` for wasm32-wasip2, then the artifact copied next
# to its sources, where the hosts and tests look for it.
#   build_rust <dir>
build_rust() {
  local dir=examples/plugins/$1 name=${1//-/_}
  cargo build --manifest-path "$dir/Cargo.toml" --target wasm32-wasip2 --release
  cp "$dir/target/wasm32-wasip2/release/$name.wasm" "$dir/$name.wasm"
}

# A JavaScript guest: ComponentizeJS, driven by the package's own build script.
#   build_js <dir>
build_js() {
  (cd "examples/plugins/$1" && npm install --silent && npm run --silent build)
}

# A Python guest: componentize-py in a venv the directory keeps.
#   build_py <dir> <wit dir, relative to the guest> <world>
build_py() {
  local dir=examples/plugins/$1 name=${1//-/_}
  (
    cd "$dir"
    [ -d .venv ] || python3 -m venv .venv
    ./.venv/bin/pip install --quiet componentize-py
    ./.venv/bin/componentize-py -d "$2" -w "$3" componentize app -o "$name.wasm"
  )
}

# A C++ guest, with wasi-sdk.
#
# The bindings are regenerated every build: they are a function of the WIT, so
# a stale copy is a silent disagreement with the world everyone else compiled.
# They are C, and are compiled with clang: compiling them with clang++ mangles
# the component-type force-link symbol and the link fails on a name nothing
# explains. `wasm32-wasip2-clang++` emits a component directly.
#   build_cpp <dir> <wit dir, relative to the guest> <world> <source.cc> [extra flags]...
build_cpp() {
  local dir=examples/plugins/$1 wit=$2 world=$3 source=$4 name=${1//-/_}
  shift 4
  local bindings=${world//-/_}
  (
    cd "$dir"
    wit-bindgen c "$wit" --world "$world" --out-dir bindings >/dev/null
    "$sdk/bin/wasm32-wasip2-clang" -std=c11 -O2 -I. -c "bindings/$bindings.c" -o bindings.o
    "$sdk/bin/wasm32-wasip2-clang++" -std=c++20 -O2 -fno-exceptions -fno-rtti -I. "$@" \
      -o "$name.wasm" "$source" bindings.o "bindings/${bindings}_component_type.o"
  )
}

# ---------------------------------------------------------------------------
# The targets
# ---------------------------------------------------------------------------

# One function per target, so the table below reads as a list of what exists
# and each row's reason for existing sits with its build line. The directory
# under examples/plugins is the target name; the `.wasm` is the name with `_`.
target_rust() { build_rust rust-lint; }
# A second Rust guest, and the only sample that needs a capability: `lut`
# opens its lookup table itself.
target_rust_asset() { build_rust rust-asset; }
# The preview world's reference guest: a QOI decoder, the codec on the
# untrusted side.
target_rust_qoi() { build_rust rust-qoi; }
# The preview world's second format, so that a previewer with two decoders
# installed is dispatching between two codecs rather than two copies of one.
target_rust_farbfeld() { build_rust rust-farbfeld; }

target_js() { build_js js-lint; }
# `js-asset` compiles against its own `wit/`, a world that `include`s
# `asset-plugin` and adds the two `wasi:filesystem` imports `lut` needs:
# StarlingMonkey exposes no file API to JavaScript and a guest may only import
# what its world declares. `wit/js-asset.wit` says so at length.
target_js_asset() { build_js js-asset; }
# The third QOI decoder. Its bill is the JS engine's, not the decoder's:
# StarlingMonkey links the wall clock and the filesystem whether or not the
# script touches them.
target_js_qoi() { build_js js-qoi; }

target_py() { build_py py-lint ../../wit/lint lint-plugin; }
# The one asset guest that needs nothing added to the world: CPython is linked
# against wasi-libc, which does the preopen prefix match inside `open`.
target_py_asset() { build_py py-asset ../../wit/asset asset-plugin; }
# CPython links every wasi:sockets interface at startup, which is the whole
# reason `net = "linked"` exists.
target_py_qoi() { build_py py-qoi ../../wit/preview decoder; }

target_cpp() { build_cpp cpp-lint ../../wit/lint lint-plugin lint.cc; }
# The C++ guest that has to agree with rust-asset on every output byte.
# `-ffp-contract=off`: `gain` is a multiply and an add, which is exactly the
# shape a compiler is allowed to fuse; wasm has no fma instruction so it cannot
# happen here, and the flag says so out loud rather than relying on the target.
target_cpp_asset() { build_cpp cpp-asset ../../wit/asset asset-plugin asset.cc -ffp-contract=off; }
# Notable for what it does *not* import: no clock, because it never reaches for
# one and wasi-libc links only what is touched. Compare rust-qoi.
target_cpp_qoi() { build_cpp cpp-qoi ../../wit/preview decoder qoi.cc; }

# name | toolchain
table='
rust          rust
rust-asset    rust
rust-qoi      rust
rust-farbfeld rust
js            js
js-asset      js
js-qoi        js
py            py
py-asset      py
py-qoi        py
cpp           cpp
cpp-asset     cpp
cpp-qoi       cpp
'

# Whether the toolchain for a row is present; sets `skip_reason` when not.
toolchain_ready() {
  case $1 in
    rust) have_rust_target || { skip_reason="rustup target add wasm32-wasip2"; return 1; } ;;
    js) have_npm || { skip_reason="npm not found"; return 1; } ;;
    py) have_python || { skip_reason="python3 not found"; return 1; } ;;
    cpp) have_wasi_sdk || { skip_reason=$cpp_skip; return 1; } ;;
  esac
}

if [ "${1:-}" = "--list" ]; then
  echo "$table" | awk 'NF { print $1 }'
  exit 0
fi

requested=("$@")
for name in "${requested[@]}"; do
  echo "$table" | awk -v n="$name" '$1 == n { found = 1 } END { exit !found }' ||
    { echo "unknown target $name; tools/build-plugins.sh --list names them" >&2; exit 2; }
done

wanted() {
  [ "${#requested[@]}" -eq 0 ] && return 0
  local r
  for r in "${requested[@]}"; do [ "$r" = "$1" ] && return 0; done
  return 1
}

failed=0
while read -r name toolchain; do
  [ -n "$name" ] || continue
  wanted "$name" || continue
  if ! toolchain_ready "$toolchain"; then
    if [ "${#requested[@]}" -eq 0 ]; then
      echo "skip $name: $skip_reason"
    else
      echo "cannot build $name: $skip_reason" >&2
      failed=1
    fi
    continue
  fi
  echo "==> $name"
  "target_${name//-/_}"
done <<<"$table"

echo
echo "built components:"
ls -1 examples/plugins/*/*.wasm 2>/dev/null || echo "  (none)"
exit $failed
