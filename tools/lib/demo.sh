# Shared by tools/demo.sh and tools/demo-preview.sh. Source it; do not run it.
#
# Three printers, and the signing block both demos open with. The demos stay
# separate scripts because they tell different stories; what they share is the
# part that is not the story.

bold() { printf '\n\033[1m%s\033[0m\n' "$*"; }
dim() { printf '\033[2m%s\033[0m\n' "$*"; }
step() { printf '\n\033[1;36m── %s\033[0m\n\n' "$*"; }

# Sign the given components with a throwaway key and write a copy of `$policy`
# that trusts it to `$work/signed.toml`.
#
# On success sets `policy` to that file and `signed=yes`. openssl produces
# exactly what `cosign sign-blob --key` does -- ECDSA P-256 over SHA-256,
# base64 -- and is far likelier to be installed. Without it the demo still
# runs; it just runs unsigned, and says so, which is the true state of affairs
# rather than a hidden fallback.
#
# The `.sig` files land next to the committed components, so the cleanup trap
# is widened to cover them *before* the first one is written: a failure between
# the write and the trap would otherwise leave a stray `.sig` in the tree.
#
#   sign_demo_components <component>...
sign_demo_components() {
  signed=""
  command -v openssl >/dev/null 2>&1 || return 0

  local sigs=()
  local component
  for component in "$@"; do
    sigs+=("$component.sig")
  done
  # shellcheck disable=SC2064  # expand now: these names are fixed here
  trap "rm -rf '$work' ${sigs[*]@Q}" EXIT

  openssl ecparam -name prime256v1 -genkey -noout -out "$work/demo.key" 2>/dev/null
  openssl ec -in "$work/demo.key" -pubout -out "$work/demo.pub" 2>/dev/null
  for component in "$@"; do
    openssl dgst -sha256 -sign "$work/demo.key" -out "$work/sig.der" "$component" 2>/dev/null
    openssl base64 -A -in "$work/sig.der" -out "$component.sig" 2>/dev/null
  done
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
}

# Copy `$1` to `$work/tampered.wasm` with its signature beside it, then flip one
# byte in the middle, well past the header. The point is that the signature
# covers the bytes, not that this particular edit is meaningful.
tamper_demo_component() {
  cp "$1" "$work/tampered.wasm"
  cp "$1.sig" "$work/tampered.wasm.sig"
  printf 'X' | dd of="$work/tampered.wasm" bs=1 seek=4096 conv=notrunc 2>/dev/null
}
