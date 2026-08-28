#!/usr/bin/env bash
# Format the C/C++ sources with clang-format (Google style, see .clang-format).
#
#   tools/format.sh           rewrite in place
#   tools/format.sh --check   fail if anything is unformatted (for CI)
set -euo pipefail
cd "$(dirname "$0")/.."

files=()
while IFS= read -r f; do files+=("$f"); done < <(
  git ls-files --cached --others --exclude-standard \
    '*.c' '*.h' '*.cc' '*.hpp')
if [ "${#files[@]}" -eq 0 ]; then
  echo "no C/C++ sources to format"
  exit 0
fi

fmt=${CLANG_FORMAT:-clang-format}
if ! command -v "$fmt" >/dev/null 2>&1; then
  echo "clang-format not found (brew install clang-format)" >&2
  exit 1
fi

if [ "${1:-}" = "--check" ]; then
  "$fmt" --dry-run --Werror "${files[@]}"
else
  "$fmt" -i "${files[@]}"
fi
