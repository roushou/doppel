#!/usr/bin/env bash
# Every check a commit must pass, in one place: formatting first, so
# unformatted code fails the same way a failing test does.
#   scripts/check.sh
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

step() { printf '\n== %s\n' "$*"; }

step "rustfmt"
cargo fmt --all --check
step "dprint (Markdown, TOML, JSON, YAML)"
dprint check
step "buf lint"
buf lint
step "clippy"
cargo clippy --workspace --all-targets --quiet -- -D warnings
step "clippy (WebAssembly: the viewer in a browser)"
cargo clippy -p doppel-viewer --target wasm32-unknown-unknown --all-targets --quiet -- -D warnings
step "cargo test"
cargo test --workspace --quiet
step "swift test"
swift test --package-path apple --quiet
printf '\nall checks passed\n'
