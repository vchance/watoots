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

# Sign the decoders, so the rest of the demo runs verified. A codec is
# precisely the thing you want to know came from who you think -- it parses
# what the user downloaded -- so the flagship example should carry the
# flagship check rather than leave it to the linter's demo.
#
# openssl produces exactly what `cosign sign-blob --key` does. Without it the
# demo still runs, unsigned, and says so.
signed=""
farbfeld=examples/plugins/rust-farbfeld/rust_farbfeld.wasm
[ -f "$farbfeld" ] || tools/build-plugins.sh rust-farbfeld >/dev/null
if command -v openssl >/dev/null 2>&1; then
  openssl ecparam -name prime256v1 -genkey -noout -out "$work/demo.key" 2>/dev/null
  openssl ec -in "$work/demo.key" -pubout -out "$work/demo.pub" 2>/dev/null
  for component in "$decoder" "$farbfeld"; do
    openssl dgst -sha256 -sign "$work/demo.key" -out "$work/sig.der" "$component" 2>/dev/null
    openssl base64 -A -in "$work/sig.der" -out "$component.sig" 2>/dev/null
  done
  {
    sed '/^\[signature\]/,$d' "$policy"
    printf '\n[signature]\nkeys = ["""\n'
    cat "$work/demo.pub"
    printf '"""]\n'
  } > "$work/signed.toml"
  policy="$work/signed.toml"
  signed=yes
  trap 'rm -rf "$work" "$decoder.sig" "$farbfeld.sig"' EXIT
fi

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
dim "\$ host_cpp_preview policy.toml blocks.ff out.png rust_qoi.wasm rust_farbfeld.wasm"
echo
$preview "$policy" "$fixtures/blocks.ff" "$work/ff.png" "$decoder" "$farbfeld"
echo
dim "A farbfeld file, with the QOI decoder installed first. It looked at the"
dim "magic bytes, said no, and the farbfeld decoder said yes. Two formats, one"
dim "policy, one host that never learned either format exists."

# ---------------------------------------------------------------------------
if [ -n "$signed" ]; then
  step "6. And they have to be the codecs you signed"
  dim "Every step above ran verified: both decoders were signed with a throwaway"
  dim "key and the policy lists its public half. Replace one byte of a codec and:"
  echo
  cp "$decoder" "$work/tampered.wasm"
  cp "$decoder.sig" "$work/tampered.wasm.sig"
  printf 'X' | dd of="$work/tampered.wasm" bs=1 seek=4096 conv=notrunc 2>/dev/null
  dim "\$ host_cpp_preview policy.toml blocks.qoi out.png tampered.wasm"
  echo
  if $preview "$policy" "$fixtures/blocks.qoi" "$work/t.png" "$work/tampered.wasm"; then
    echo "UNEXPECTED: a tampered decoder should not load"
    exit 1
  fi
  echo
  dim "The permission check would have let it in: it asks for nothing new."
  dim "Only the signature can tell that this is not the codec you approved --"
  dim "and a codec is the plugin most worth being sure about."
fi

bold "That is what the sandbox is for."
dim "Code you did not write, on input you do not trust, inside your process --"
dim "and a policy you can read that says exactly how far it can get."
echo
