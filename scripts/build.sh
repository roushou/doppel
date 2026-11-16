#!/usr/bin/env bash
# Builds `doppel` (Rust), `doppel-vmm` and `doppel-agent` (Swift), signs
# doppel-vmm with the virtualization entitlement, and places both next to doppel.
# doppel-agent runs inside the guest: `doppel agent install <machine>` puts
# doppel-agent.zip (Doppel Agent.app) there, or copy the bare binary by hand.
#   scripts/build.sh            debug
#   scripts/build.sh --release  release
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
profile=debug; cargo_flags=(); swift_config=debug
if [[ "${1:-}" == "--release" ]]; then profile=release; cargo_flags=(--release); swift_config=release; fi

# The browser viewer's page, which the gateway embeds.
(cd "$root/web" && pnpm install --frozen-lockfile --silent && pnpm run --silent build)
cargo build --manifest-path "$root/Cargo.toml" ${cargo_flags[@]+"${cargo_flags[@]}"} -p doppel-cli
swift build --package-path "$root/apple" -c "$swift_config" --product doppel-vmm
swift build --package-path "$root/apple" -c "$swift_config" --product doppel-agent
cp "$root/apple/.build/$swift_config/doppel-agent" "$root/target/$profile/doppel-agent"

out="$root/target/$profile/doppel-vmm"
cp "$root/apple/.build/$swift_config/doppel-vmm" "$out"
# Sign with a stable identity so the guest agent keeps its Accessibility and
# Screen Recording grants across rebuilds: TCC matches an ad-hoc build by its
# cdhash, which every rebuild changes. DOPPEL_SIGNING_IDENTITY wins; otherwise
# the keychain's Apple Development identity is used when there is exactly one.
# Ad-hoc signing still builds and runs, but each new agent needs its
# grants again.
identity="${DOPPEL_SIGNING_IDENTITY:-}"
if [[ -z "$identity" ]]; then
  found="$(security find-identity -v -p codesigning | awk '/"Apple Development: /{print $2}')"
  if [[ -n "$found" && "$(wc -l <<<"$found")" -eq 1 ]]; then
    identity="$found"
  else
    identity="-"
    echo "warning: ad-hoc signing (set DOPPEL_SIGNING_IDENTITY): a rebuilt agent loses its guest grants" >&2
  fi
fi
codesign --force --sign "$identity" --entitlements "$root/resources/doppel-vmm.entitlements" "$out"
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
codesign --force --sign "$identity" --identifier dev.doppel.agent "$app"
codesign --verify --strict "$app"
rm -f "$root/target/$profile/doppel-agent.zip"
(cd "$root/target/$profile" && ditto -c -k --keepParent "Doppel Agent.app" doppel-agent.zip)
printf '%s\n' "$build_id" > "$root/target/$profile/doppel-agent.build-id"

echo "built $root/target/$profile/{doppel,doppel-vmm,doppel-agent,doppel-agent.zip} (signed: $identity; agent build $build_id)"
