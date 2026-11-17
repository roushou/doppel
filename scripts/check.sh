#!/usr/bin/env bash
# Every check a commit must pass, in one place: formatting first, so
# unformatted code fails the same way a failing test does.
#   scripts/check.sh
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

step() { printf '\n== %s\n' "$*"; }

step "web: install, types, tests, build (embedded by doppel-gateway)"
(cd web && pnpm install --frozen-lockfile --silent && pnpm run --silent check && pnpm run --silent test && pnpm run --silent build)
step "rustfmt"
cargo fmt --all --check
step "dprint (Markdown, TOML, JSON, YAML)"
dprint check
step "buf lint"
buf lint
step "clippy"
cargo clippy --workspace --all-targets --quiet -- -D warnings
step "clippy (WebAssembly: the browser viewer's film)"
cargo clippy -p doppel-browser-film --target wasm32-unknown-unknown --quiet -- -D warnings
step "cargo test"
cargo test --workspace --quiet
step "swift test"
swift test --package-path apple --quiet
printf '\nall checks passed\n'
