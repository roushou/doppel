#!/usr/bin/env bash
# Builds the viewer for the browser into target/viewer-web, which
# doppel-gateway embeds: the viewer as WebAssembly, its JavaScript
# bindings, and the page that runs it.
#   scripts/build-web.sh            release
#   scripts/build-web.sh --debug    debug: faster to build, slower to run
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
profile=release; cargo_flags=(--release)
if [[ "${1:-}" == "--debug" ]]; then profile=debug; cargo_flags=(); fi

want="$(cargo metadata --manifest-path "$root/Cargo.toml" --format-version 1 \
  | python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "wasm-bindgen"))')"
if [[ "$(wasm-bindgen --version 2>/dev/null)" != "wasm-bindgen $want" ]]; then
  echo "needs wasm-bindgen $want: cargo install wasm-bindgen-cli --version $want --locked" >&2
  exit 1
fi

cargo rustc --manifest-path "$root/Cargo.toml" ${cargo_flags[@]+"${cargo_flags[@]}"} \
  -p doppel-viewer --lib --target wasm32-unknown-unknown --crate-type cdylib
out="$root/target/viewer-web"
rm -rf "$out"
wasm-bindgen --target web --no-typescript --out-dir "$out" \
  "$root/target/wasm32-unknown-unknown/$profile/doppel_viewer.wasm"
cp "$root/crates/doppel-viewer/page/"* "$out/"
echo "built $out"
