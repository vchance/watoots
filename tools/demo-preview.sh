#!/usr/bin/env bash
# The previewer demo: a file-format decoder as an untrusted plugin.
#
#   tools/demo-preview.sh
#
# Everything below is real output. The decoder is compiled from
# examples/plugins/rust-qoi, the host from examples/host-cpp-preview, and the
# hostile file is a real QOI whose header claims a 1 GiB image.
set -euo pipefail
cd "$(dirname "$0")/.."

bold() { printf '\n\033[1m%s\033[0m\n' "$*"; }
dim() { printf '\033[2m%s\033[0m\n' "$*"; }
step() { printf '\n\033[1;36m── %s\033[0m\n\n' "$*"; }

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

decoder=examples/plugins/rust-qoi/rust_qoi.wasm
policy=examples/policies/rust-qoi.toml
fixtures=examples/fixtures/preview
watoots=target/release/watoots
preview=build/dev/examples/host-cpp-preview/host_cpp_preview

bold "Building"
[ -f "$decoder" ] || tools/build-plugins.sh rust-qoi >/dev/null
cargo build --release -p watoots-cli >/dev/null 2>&1
[ -x "$preview" ] || { cmake --preset dev >/dev/null && cmake --build --preset dev --target host_cpp_preview >/dev/null; }
dim "decoder $(wc -c <"$decoder" | tr -d ' ') bytes; previewer is C++ over the C API"

# ---------------------------------------------------------------------------
step "1. What does a codec need?"
dim "\$ watoots inspect rust_qoi.wasm"
echo
$watoots inspect "$decoder" 2>&1 | sed -n '1,8p' || true
echo
dim "Nothing the codec asked for. The two DENYs are Rust's std linking a clock"
dim "and the environment whether or not the author touched them. No filesystem,"
dim "no network: a decoder reads the bytes it is handed and returns pixels."

# ---------------------------------------------------------------------------
step "2. Open a file"
dim "\$ host_cpp_preview policy.toml blocks.qoi out.png rust_qoi.wasm"
echo
$preview "$policy" "$fixtures/blocks.qoi" "$work/out.png" "$decoder"
dim "A real PNG, decoded by code this process never trusted."

# ---------------------------------------------------------------------------
step "3. Open a hostile file"
dim "bomb.qoi is blocks.qoi with the header rewritten to claim 16384 x 16384."
dim "Nothing else is wrong with it. The decoder's own checks pass -- it is not"
dim "corrupt -- and it asks for 1 GiB of pixels."
echo
dim "\$ host_cpp_preview policy.toml bomb.qoi out.png rust_qoi.wasm"
echo
if $preview "$policy" "$fixtures/bomb.qoi" "$work/bomb.png" "$decoder"; then
  echo "UNEXPECTED: the bomb should not decode"
  exit 1
fi
echo
dim "The policy says memory = \"64MiB\". The sandbox refused the allocation, the"
dim "host got a clean error naming the ceiling, and the process is still running."
dim "With the codec in-process this is an OOM kill -- or, with a less honest"
dim "header, the CVE. One manifest line."

# ---------------------------------------------------------------------------
step "4. The same bug, as a file"
dim "\$ watoots record rust_qoi.wasm -m policy.toml -c decode -o bug.wave -- <bomb bytes>"
echo
bytes=$(python3 -c "print('['+','.join(map(str,open('$fixtures/bomb.qoi','rb').read()))+']')")
$watoots record "$decoder" -m "$policy" -c decode -o "$work/bug.wave" -- "$bytes" 2>&1 | grep -E "^(watoots:|wrote)" | sed "s|$work/||" || true
echo
dim "\$ watoots replay bug.wave -c rust_qoi.wasm --assert"
echo
$watoots replay "$work/bug.wave" -c "$decoder" --assert 2>&1 | head -2 || true
echo
dim "The trace carries the offending bytes as the argument. Whoever gets the bug"
dim "report reproduces it with no viewer, no policy file and no fixtures -- and"
dim "--emit-test turns it into a regression test."

# ---------------------------------------------------------------------------
step "5. Two codecs installed"
cpp=examples/plugins/cpp-qoi/cpp_qoi.wasm
if [ -f "$cpp" ]; then
  dim "\$ host_cpp_preview policy.toml blocks.qoi out.png cpp_qoi.wasm rust_qoi.wasm"
  echo
  $preview "$policy" "$fixtures/blocks.qoi" "$work/two.png" "$cpp" "$decoder"
  dim "The C++ decoder answered first. Same world, same policy, same bytes out;"
  dim "the host was not recompiled. C++ as the untrusted side is the half of"
  dim "\"C++ has no component-model plugin option\" that a C++ host cannot show."
else
  dim "(build the C++ decoder to see this: tools/build-plugins.sh cpp-qoi)"
fi

bold "That is what the sandbox is for."
dim "Code you did not write, on input you do not trust, inside your process --"
dim "and a policy you can read that says exactly how far it can get."
echo
