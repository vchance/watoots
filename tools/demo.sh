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

bold "That is the whole product."
dim "A manifest you can review before installing, and a bug report that is a"
dim "file, which becomes a regression test with no host code around it."
echo
