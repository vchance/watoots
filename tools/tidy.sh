#!/usr/bin/env bash
# Run clang-tidy over the C/C++ sources (Google checks, see .clang-tidy).
#
#   tools/tidy.sh
#
# Two bits of friction this script exists to absorb:
#
#   * Homebrew's llvm formula is keg-only, so clang-tidy is not on PATH even
#     when installed.
#   * clang-tidy parses with its own clang, which on macOS is a different major
#     version from the Apple clang that CMake picked -- it then cannot find the
#     standard library and reports "'string' file not found" plus a cascade of
#     nonsense. So we configure a build tree with the compilers that ship
#     alongside the clang-tidy we found, purely to get a matching compile
#     database. Set WATOOTS_BUILD_DIR to put that tree somewhere else.
#
# The tree is reconfigured every run. CMake is incremental so it costs little,
# and a stale compile database silently drops translation units -- which is how
# this script once passed locally while CI failed at the same commit.
set -euo pipefail
cd "$(dirname "$0")/.."

# See tools/lib/llvm.sh for why the major is pinned.
. tools/lib/llvm.sh
if ! tidy=$(find_llvm_tool clang-tidy "${CLANG_TIDY:-}"); then
  echo "clang-tidy not found (brew install llvm@22)" >&2
  exit 1
fi
warn_unless_llvm_22 "$tidy" "the check set"

bin_dir=$(dirname "$tidy")
build_dir=${WATOOTS_BUILD_DIR:-build/tidy}

configure=(cmake -S . -B "$build_dir" -G Ninja
  -DCMAKE_BUILD_TYPE=Debug
  -DCMAKE_EXPORT_COMPILE_COMMANDS=ON
  -DWATOOTS_BUILD_TESTS=ON)
if [ -x "$bin_dir/clang++" ]; then
  configure+=(-DCMAKE_C_COMPILER="$bin_dir/clang"
    -DCMAKE_CXX_COMPILER="$bin_dir/clang++")
fi
"${configure[@]}" >/dev/null

runner="$bin_dir/run-clang-tidy"
# Every example host and the header they share are in scope: the pattern is
# every `examples/host-cpp*` directory plus `examples/common`, so a fourth host
# is covered the day it is added rather than the day someone notices. The
# first version of this line named two hosts and silently skipped the third,
# which happened to be the headline one. `examples/host-cpp-asset/stb_impl.cc`
# is linted like everything else and passes trivially: it is two macros and two
# includes, and CMake pulls stb in with SYSTEM, so every diagnostic those
# headers would raise is suppressed at its own -isystem location rather than by
# a NOLINT here.
sources='(crates/host-capi/(include|src|tests)|examples/(host-cpp[a-z-]*|common))/.*\.(c|cc|h|hpp)$'

if [ -x "$runner" ]; then
  exec "$runner" -p "$build_dir" -clang-tidy-binary "$tidy" -quiet "$sources"
fi

files=()
while IFS= read -r f; do files+=("$f"); done < <(
  git ls-files --cached --others --exclude-standard \
    'crates/host-capi/*.c' 'crates/host-capi/*.cc' 'examples/host-cpp*/*.cc' \
    'examples/common/*.hpp')
exec "$tidy" -p "$build_dir" "${files[@]}"
