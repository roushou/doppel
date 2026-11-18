#!/usr/bin/env bash
# Builds the console for the browser into target/console-web, which
# doppel-gateway embeds: the console as WebAssembly, its JavaScript
# bindings, and the page that runs it, each with compressed copies.
#   scripts/build-web.sh            built for size (the `web` profile)
#   scripts/build-web.sh --debug    debug: faster to build, slower to run
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
profile=web; cargo_flags=(--profile web)
if [[ "${1:-}" == "--debug" ]]; then profile=debug; cargo_flags=(); fi

want="$(cargo metadata --manifest-path "$root/Cargo.toml" --format-version 1 \
  | python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "wasm-bindgen"))')"
if [[ "$(wasm-bindgen --version 2>/dev/null)" != "wasm-bindgen $want" ]]; then
  echo "needs wasm-bindgen $want: cargo install wasm-bindgen-cli --version $want --locked" >&2
  exit 1
fi

cargo rustc --manifest-path "$root/Cargo.toml" ${cargo_flags[@]+"${cargo_flags[@]}"} \
  -p doppel-console --lib --target wasm32-unknown-unknown --crate-type cdylib
out="$root/target/console-web"
rm -rf "$out"
wasm-bindgen --target web --no-typescript --remove-name-section --remove-producers-section --out-dir "$out" \
  "$root/target/wasm32-unknown-unknown/$profile/doppel_console.wasm"
cp "$root/crates/doppel-console/page/"* "$out/"
# Compressed copies, which the gateway sends to browsers that take them.
find "$out" -type f \( -name '*.wasm' -o -name '*.js' -o -name '*.html' -o -name '*.css' \) \
  -exec gzip -9 -k -n {} \;
if command -v brotli >/dev/null; then
  find "$out" -type f \( -name '*.wasm' -o -name '*.js' -o -name '*.html' -o -name '*.css' \) \
    -exec brotli -q 11 -k {} \;
else
  echo "warning: no brotli (brew install brotli): browsers get gzip, about a third bigger" >&2
fi
echo "built $out"
