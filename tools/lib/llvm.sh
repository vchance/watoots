# Shared by tools/format.sh and tools/tidy.sh. Source it; do not run it.
#
# LLVM 22 is the pinned major (ADR-0003): `BasedOnStyle: Google` is not frozen
# across clang-format releases, and `.clang-tidy` sets `WarningsAsErrors: '*'`,
# so an unpinned tool is a source of spurious red on one machine and silence on
# another. Homebrew's llvm formulae are keg-only, so nothing is on PATH even
# when installed.

# Print the path of an LLVM tool, preferring a 22. Honour an explicit override
# first, then the versioned package locations, then whatever is on PATH, then
# an unversioned Homebrew keg.
#
#   find_llvm_tool clang-format "$CLANG_FORMAT"
find_llvm_tool() {
  local name=$1 override=${2:-}
  if [ -n "$override" ]; then
    echo "$override"
    return 0
  fi
  local candidate
  for candidate in \
    "/opt/homebrew/opt/llvm@22/bin/$name" \
    "/usr/local/opt/llvm@22/bin/$name" \
    "/usr/lib/llvm-22/bin/$name" \
    "$(command -v "$name-22" 2>/dev/null || true)" \
    "$(command -v "$name" 2>/dev/null || true)" \
    "/opt/homebrew/opt/llvm/bin/$name" \
    "/usr/local/opt/llvm/bin/$name"; do
    if [ -n "$candidate" ] && [ -x "$candidate" ]; then
      echo "$candidate"
      return 0
    fi
  done
  return 1
}

# Warn when a found tool is not the pinned major.
warn_unless_llvm_22() {
  local tool=$1 what=$2
  case "$("$tool" --version)" in
    *"version 22."*) ;;
    *)
      echo "warning: $tool is not LLVM 22 ($("$tool" --version | grep -m1 version))," >&2
      echo "         but CI pins LLVM 22; $what may disagree." >&2
      ;;
  esac
}
