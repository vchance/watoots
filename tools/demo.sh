#!/usr/bin/env bash
# The watoots demo: load a plugin, deny a permission, record a bug, replay it.
#
#   tools/demo.sh
#
# Everything below is real output from the tools in this repo. Nothing is
# staged: the plugin is compiled from examples/plugins/rust-lint.
set -euo pipefail
cd "$(dirname "$0")/.."

bold() { printf '\n\033[1m%s\033[0m\n' "$*"; }
dim() { printf '\033[2m%s\033[0m\n' "$*"; }
step() { printf '\n\033[1;36m── %s\033[0m\n\n' "$*"; }

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

plugin=examples/plugins/rust-lint/rust_lint.wasm
policy=examples/policies/rust-lint.toml
watoots=target/release/watoots

bold "Building"
[ -f "$plugin" ] || tools/build-plugins.sh rust >/dev/null
cargo build --release -p watoots-cli >/dev/null 2>&1
dim "$($watoots --version), plugin $(wc -c <"$plugin" | tr -d ' ') bytes"

# Sign the plugin, so the rest of the demo runs verified rather than printing a
# warning at every step. This is also the shortest honest demonstration of the
# feature: the same key that makes step 1 quiet makes step 8 refuse a tamper.
#
# openssl produces exactly what `cosign sign-blob --key` does -- ECDSA P-256
# over SHA-256, base64 -- and is far likelier to be installed. If it is missing
# the demo still runs; it just runs unsigned, and says so, which is the true
# state of affairs rather than a hidden fallback.
signed=""
if command -v openssl >/dev/null 2>&1; then
  openssl ecparam -name prime256v1 -genkey -noout -out "$work/demo.key" 2>/dev/null
  openssl ec -in "$work/demo.key" -pubout -out "$work/demo.pub" 2>/dev/null
  openssl dgst -sha256 -sign "$work/demo.key" -out "$work/plugin.der" "$plugin" 2>/dev/null
  openssl base64 -A -in "$work/plugin.der" -out "$plugin.sig" 2>/dev/null
  {
    # The shipped policy ends with its own `[signature] required = false`;
    # drop it rather than appending a second section and colliding.
    sed '/^\[signature\]/,$d' "$policy"
    printf '\n[signature]\nkeys = ["""\n'
    cat "$work/demo.pub"
    printf '"""]\n'
  } > "$work/signed.toml"
  policy="$work/signed.toml"
  signed=yes
  trap 'rm -rf "$work" "$plugin.sig"' EXIT
fi

# ---------------------------------------------------------------------------
step "1. Load a plugin and call it"
dim "\$ watoots run $plugin -m $policy -c lint -- '\"notes.md\"' ..."
echo
$watoots run "$plugin" -m "$policy" \
  --answer 'watoots:example/log@0.1.0#emit=' \
  -c lint -- '"notes.md"' '"TODO: ship it\ntrailing   \n"'

# ---------------------------------------------------------------------------
step "2. Deny a permission"
cat > "$work/tight.toml" <<'TOML'
# Same plugin, a policy that grants nothing.
[limits]
fuel = 200_000_000
TOML
dim "\$ watoots inspect $plugin -m tight.toml"
echo
# Denials mean a non-zero exit, which is the point; keep going anyway.
$watoots inspect "$plugin" -m "$work/tight.toml" || true
echo
dim "No guest code ran. The component declares its imports in the binary,"
dim "so this is a load-time answer, not a runtime trap -- and the exit code is"
dim "non-zero, so it works as a gate in CI."

# ---------------------------------------------------------------------------
step "3. Record a session"
dim "\$ watoots record ... -o bug.wave"
echo
$watoots record "$plugin" -m "$policy" \
  --answer 'watoots:example/log@0.1.0#emit=' \
  -c lint -o "$work/bug.wave" \
  -- '"notes.md"' '"TODO: ship it\n"' >/dev/null
echo
cat "$work/bug.wave" | sed -n '/^export-call/,$p'
dim "(the manifest travels in the header, above)"

# ---------------------------------------------------------------------------
step "4. Replay it, with no application present"
dim "\$ watoots replay bug.wave -c $plugin --assert"
echo
$watoots replay "$work/bug.wave" -c "$plugin" --assert
dim "exit $?"

step "5. Now break it"
dim "The trace is text, so a reviewer can edit it -- and CI will notice."
sed 's/1 diagnostic(s)/9 diagnostic(s)/' "$work/bug.wave" > "$work/edited.wave"
echo
dim "\$ watoots replay edited.wave -c $plugin --assert"
echo
if $watoots replay "$work/edited.wave" -c "$plugin" --assert; then
  echo "UNEXPECTED: the edit was not caught"
  exit 1
else
  dim "exit 1 -- a divergence fails the build"
fi

# ---------------------------------------------------------------------------
step "6. Where the time actually goes"
dim "\$ watoots profile $plugin -m $policy -c lint --repeat 200"
echo
$watoots profile "$plugin" -m "$policy" \
  --answer 'watoots:example/log@0.1.0#emit=' \
  -c lint --repeat 200 -- '"notes.md"' '"TODO: ship it\n"' | head -12
echo
dim "Three buckets, not one number. A slow call is a slow plugin, a slow host"
dim "function, or a value being copied across the boundary -- and the fix is"
dim "different in each case."

# ---------------------------------------------------------------------------
step "7. Swap the plugin without restarting the host"
dim "\$ watoots reload $plugin --to rust_lint.wasm -c name"
echo
$watoots reload "$plugin" --to "$plugin" -m "$policy" \
  --answer 'watoots:example/log@0.1.0#emit=' -c name
echo
dim "Same world, new bytes, no restart. The replacement is built and checked"
dim "before the running plugin is touched."

# ---------------------------------------------------------------------------
step "8. And refused when the new build wants more"
# Not a contrived failure: the C++ guest is a real build of the same world, and
# wasi-libc links the wall clock where Rust's std does not. The manifest that
# was right for the old bytes is wrong for the new ones, and that is the case
# reload exists to catch.
cpp=examples/plugins/cpp-lint/cpp_lint.wasm
if [ -f "$cpp" ]; then
  dim "\$ watoots reload $plugin --to cpp_lint.wasm -c name"
  echo
  if $watoots reload "$plugin" --to "$cpp" -m "$policy" \
    --answer 'watoots:example/log@0.1.0#emit=' -c name 2>&1 | head -2; then
    echo
    echo "UNEXPECTED: the C++ build should want the wall clock"
    exit 1
  fi
  echo
  dim "The C++ build links wasi:clocks/wall-clock through wasi-libc; the policy"
  dim "grants monotonic. A plugin must not acquire a capability by being updated,"
  dim "so the reload is refused and the Rust plugin is still the one running."
else
  dim "(build the C++ guest to see this: tools/build-plugins.sh cpp)"
fi

# ---------------------------------------------------------------------------
if [ -n "$signed" ]; then
  step "8. And it has to be the plugin you signed"
  dim "Every step above ran verified -- the plugin was signed with a throwaway"
  dim "key and the policy lists its public half. Change one byte and:"
  echo
  cp "$plugin" "$work/tampered.wasm"
  cp "$plugin.sig" "$work/tampered.wasm.sig"
  # Flip a byte in the middle, well past the header. The point is that the
  # signature covers the bytes, not that this particular edit is meaningful.
  printf 'X' | dd of="$work/tampered.wasm" bs=1 seek=4096 conv=notrunc 2>/dev/null
  dim "\$ watoots run tampered.wasm -m signed.toml -c name"
  echo
  if $watoots run "$work/tampered.wasm" -m "$policy" \
    --answer 'watoots:example/log@0.1.0#emit=' -c name 2>&1 | head -2; then
    echo
    echo "UNEXPECTED: a tampered plugin should not load"
    exit 1
  fi
  echo
  dim "The permission model would have let it run: it asks for nothing new."
  dim "Only the signature can tell that these are not the bytes you approved."
fi

bold "That is the whole product."
dim "A manifest you can review before installing, and a bug report that is a"
dim "file, which becomes a regression test with no host code around it."
echo
