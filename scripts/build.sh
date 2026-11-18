#!/usr/bin/env bash
# Builds `doppel` (Rust), `doppel-vmm` and `doppel-agent` (Swift), signs
# doppel-vmm with the virtualization entitlement, and places both next to doppel.
# doppel-agent runs inside the guest: `doppel agent install <machine>` puts
# doppel-agent.zip (Doppel Agent.app) there, or copy the bare binary by hand.
#   scripts/build.sh            debug
#   scripts/build.sh --release  release: also signs doppel, and signs everything
#                               with the hardened runtime and a secure timestamp,
#                               as notarization requires (scripts/release.sh)
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
profile=debug; cargo_flags=(); swift_config=debug
if [[ "${1:-}" == "--release" ]]; then profile=release; cargo_flags=(--release); swift_config=release; fi

# The console built for the browser, which the gateway embeds.
"$root/scripts/build-web.sh"
cargo build --manifest-path "$root/Cargo.toml" ${cargo_flags[@]+"${cargo_flags[@]}"} -p doppel-cli
swift build --package-path "$root/apple" -c "$swift_config" --product doppel-vmm
swift build --package-path "$root/apple" -c "$swift_config" --product doppel-agent
cp "$root/apple/.build/$swift_config/doppel-agent" "$root/target/$profile/doppel-agent"

out="$root/target/$profile/doppel-vmm"
cp "$root/apple/.build/$swift_config/doppel-vmm" "$out"
# Sign with a stable identity so the guest agent keeps its Accessibility and
# Screen Recording grants across rebuilds: TCC matches an ad-hoc build by its
# cdhash, which every rebuild changes. DOPPEL_SIGNING_IDENTITY wins; otherwise
# the keychain's Developer ID Application identity, then its Apple Development
# one, each when there is exactly one. Developer ID first: releases are signed
# with it, so a dev build's agent keeps the grants a release's was given.
# Ad-hoc signing still builds and runs, but each new agent needs its
# grants again.
identity="${DOPPEL_SIGNING_IDENTITY:-}"
for kind in "Developer ID Application" "Apple Development"; do
  if [[ -n "$identity" ]]; then break; fi
  found="$(security find-identity -v -p codesigning | awk -v kind="\"$kind: " 'index($0, kind) {print $2}')"
  if [[ -n "$found" && "$(wc -l <<<"$found")" -eq 1 ]]; then identity="$found"; fi
done
if [[ -z "$identity" ]]; then
  identity="-"
  echo "warning: ad-hoc signing (set DOPPEL_SIGNING_IDENTITY): a rebuilt agent loses its guest grants" >&2
fi
sign_flags=()
if [[ "$profile" == release ]]; then
  sign_flags=(--options runtime)
  # A timestamp needs a real identity: ad-hoc signatures have none.
  if [[ "$identity" != "-" ]]; then sign_flags+=(--timestamp); fi
  codesign --force --sign "$identity" "${sign_flags[@]}" "$root/target/$profile/doppel"
fi
codesign --force --sign "$identity" ${sign_flags[@]+"${sign_flags[@]}"} --entitlements "$root/resources/doppel-vmm.entitlements" "$out"
codesign -d --entitlements - "$out" 2>/dev/null | grep -q com.apple.security.virtualization

# The guest app: `doppel agent install <machine>` installs doppel-agent.zip as a
# LaunchAgent. The build id lets the installer tell the new agent from the old.
app="$root/target/$profile/Doppel Agent.app"
build_id="$(date -u +%Y%m%dT%H%M%SZ)-$(git -C "$root" rev-parse --short HEAD 2>/dev/null || echo nogit)"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$root/apple/.build/$swift_config/doppel-agent" "$app/Contents/MacOS/doppel-agent"
printf '%s\n' "$build_id" > "$app/Contents/Resources/build-id"
sed "s/@BUILD_ID@/$build_id/" "$root/resources/doppel-agent-Info.plist" > "$app/Contents/Info.plist"
codesign --force --sign "$identity" ${sign_flags[@]+"${sign_flags[@]}"} --identifier dev.doppel.agent "$app"
codesign --verify --strict "$app"
rm -f "$root/target/$profile/doppel-agent.zip"
(cd "$root/target/$profile" && ditto -c -k --keepParent "Doppel Agent.app" doppel-agent.zip)
printf '%s\n' "$build_id" > "$root/target/$profile/doppel-agent.build-id"
# The code requirement a guest's privacy grants hold the app to: `doppel agent
# grant` writes it into a stopped machine.
codesign -d -r- "$app" 2>/dev/null | sed -n 's/^designated => //p' |
  csreq -r- -b "$root/target/$profile/doppel-agent.csreq"

echo "built $root/target/$profile/{doppel,doppel-vmm,doppel-agent,doppel-agent.zip} (signed: $identity; agent build $build_id)"
