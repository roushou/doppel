#!/usr/bin/env bash
# Builds a release of Doppel for Apple silicon, signed with a Developer ID and
# notarized by Apple, and the Homebrew formula that installs it.
#   scripts/release.sh                sign, notarize, package
#   scripts/release.sh --no-notarize  sign and package only, to try the package
#                                     here (any identity: DOPPEL_SIGNING_IDENTITY,
#                                     else the one scripts/build.sh picks)
# Writes target/dist/: doppel-VERSION-macos-arm64.tar.gz, its .sha256, and
# doppel.rb, the formula, pointing at the tarball on the vVERSION GitHub release.
#
# Once per Mac: a "Developer ID Application" certificate in the keychain (an
# Apple Developer Program membership makes one), and the notary credentials:
#   xcrun notarytool store-credentials doppel
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
notarize=yes
if [[ "${1:-}" == "--no-notarize" ]]; then notarize=no; fi
profile="${DOPPEL_NOTARY_PROFILE:-doppel}"
repo="roushou/doppel"

version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/crates/doppel-cli/Cargo.toml" | head -1)"
name="doppel-$version-macos-arm64"
dist="$root/target/dist"

identity="${DOPPEL_SIGNING_IDENTITY:-}"
if [[ -z "$identity" ]]; then
  found="$(security find-identity -v -p codesigning | awk '/"Developer ID Application: /{print $2}')"
  if [[ -n "$found" && "$(wc -l <<<"$found")" -eq 1 ]]; then
    identity="$found"
  elif [[ "$notarize" == yes ]]; then
    echo "error: needs exactly one \"Developer ID Application\" identity in the keychain (or DOPPEL_SIGNING_IDENTITY)" >&2
    exit 1
  fi
fi
if [[ "$notarize" == yes ]]; then
  if ! git -C "$root" diff --quiet HEAD; then
    echo "error: uncommitted changes: a release is built from a commit" >&2
    exit 1
  fi
  if ! xcrun notarytool history --keychain-profile "$profile" >/dev/null 2>&1; then
    echo "error: no notary credentials in the keychain under \"$profile\": xcrun notarytool store-credentials $profile" >&2
    exit 1
  fi
fi

DOPPEL_SIGNING_IDENTITY="$identity" "$root/scripts/build.sh" --release

out="$root/target/release"
rm -rf "$dist"
mkdir -p "$dist/$name"
for file in doppel doppel-vmm doppel-agent.zip doppel-agent.build-id doppel-agent.csreq; do
  cp "$out/$file" "$dist/$name/"
done
cp "$root/LICENSE-MIT" "$root/LICENSE-APACHE" "$dist/$name/"
for program in doppel doppel-vmm; do
  codesign --verify --strict "$dist/$name/$program"
done

if [[ "$notarize" == yes ]]; then
  # The notary service takes a zip, not a tarball; its ticket covers the
  # signed programs themselves, so the tarball of the same files passes
  # Gatekeeper too.
  (cd "$dist" && ditto -c -k --keepParent "$name" "$name.zip")
  xcrun notarytool submit "$dist/$name.zip" --keychain-profile "$profile" --wait
  rm "$dist/$name.zip"
fi

tar -C "$dist" -czf "$dist/$name.tar.gz" "$name"
sha="$(shasum -a 256 "$dist/$name.tar.gz" | awk '{print $1}')"
printf '%s  %s\n' "$sha" "$name.tar.gz" > "$dist/$name.tar.gz.sha256"
rm -rf "${dist:?}/$name"

cat > "$dist/doppel.rb" <<EOF
class Doppel < Formula
  desc "Isolated macOS machines for AI agents"
  homepage "https://github.com/$repo"
  url "https://github.com/$repo/releases/download/v$version/$name.tar.gz"
  sha256 "$sha"
  license any_of: ["MIT", "Apache-2.0"]

  depends_on arch: :arm64
  depends_on :macos

  def install
    # Together, so doppel finds the VMM and the guest agent beside it.
    libexec.install "doppel", "doppel-vmm", "doppel-agent.zip", "doppel-agent.build-id", "doppel-agent.csreq"
    bin.install_symlink libexec/"doppel"
  end

  def caveats
    <<~EOS
      Make a golden image for agents (downloads macOS; takes a while):
        doppel setup
    EOS
  end

  test do
    assert_match "doppel #{version}", shell_output("#{bin}/doppel --version")
  end
end
EOF

echo "built $dist/$name.tar.gz ($([[ "$notarize" == yes ]] && echo notarized || echo not notarized); signed: ${identity:-see above})"
echo "formula: $dist/doppel.rb"
