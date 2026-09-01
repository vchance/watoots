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
#     database. Set WATOOTS_BUILD_DIR to use an existing one instead.
set -euo pipefail
cd "$(dirname "$0")/.."

tidy=${CLANG_TIDY:-}
if [ -z "$tidy" ]; then
  # LLVM 22 is the pinned major (ADR-0003). `.clang-tidy` sets
  # `WarningsAsErrors: '*'`, so the check set has to be pinned or CI and a
  # developer's machine disagree about what is an error.
  for candidate in \
    /opt/homebrew/opt/llvm@22/bin/clang-tidy \
    /usr/local/opt/llvm@22/bin/clang-tidy \
    /usr/lib/llvm-22/bin/clang-tidy \
    "$(command -v clang-tidy-22 2>/dev/null || true)" \
    "$(command -v clang-tidy 2>/dev/null || true)" \
    /opt/homebrew/opt/llvm/bin/clang-tidy \
    /usr/local/opt/llvm/bin/clang-tidy; do
    if [ -n "$candidate" ] && [ -x "$candidate" ]; then
      tidy=$candidate
      break
    fi
  done
fi
if [ -z "$tidy" ]; then
  echo "clang-tidy not found (brew install llvm)" >&2
  exit 1
fi

bin_dir=$(dirname "$tidy")
build_dir=${WATOOTS_BUILD_DIR:-build/tidy}

if [ ! -f "$build_dir/compile_commands.json" ]; then
  configure=(cmake -S . -B "$build_dir" -G Ninja
    -DCMAKE_BUILD_TYPE=Debug
    -DCMAKE_EXPORT_COMPILE_COMMANDS=ON
    -DWATOOTS_BUILD_TESTS=ON)
  if [ -x "$bin_dir/clang++" ]; then
    configure+=(-DCMAKE_C_COMPILER="$bin_dir/clang"
      -DCMAKE_CXX_COMPILER="$bin_dir/clang++")
  fi
  "${configure[@]}" >/dev/null
fi

runner="$bin_dir/run-clang-tidy"
sources='(crates/host-capi/(include|src|tests)|examples/host-cpp)/.*\.(c|cc|h|hpp)$'

if [ -x "$runner" ]; then
  exec "$runner" -p "$build_dir" -clang-tidy-binary "$tidy" -quiet "$sources"
fi

files=()
while IFS= read -r f; do files+=("$f"); done < <(
  git ls-files --cached --others --exclude-standard \
    'crates/host-capi/*.c' 'crates/host-capi/*.cc' 'examples/host-cpp/*.cc')
exec "$tidy" -p "$build_dir" "${files[@]}"
