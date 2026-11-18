#!/usr/bin/env bash
# Releases Doppel for Apple silicon: signed with a Developer ID, notarized,
# published on GitHub, and installable with Homebrew.
#
#   scripts/release.sh VERSION [--notes FILE]
#       From an up-to-date, clean main: set the workspace version to
#       VERSION and commit and push that to main, build, sign, notarize,
#       publish the vVERSION GitHub release with notes from the commits since
#       the last release (FILE's text first), update the Homebrew tap, and
#       check that `brew` installs it. Run it again after a failure: each
#       step already done is skipped.
#   scripts/release.sh --package-only
#       Sign and package the current version here, to try the package:
#       nothing is notarized, committed or published (any identity:
#       DOPPEL_SIGNING_IDENTITY, else the one scripts/build.sh picks).
#
# The version says what changed for people and programs that use Doppel:
# the CLI and its --json output, the MCP tools, the store on disk, and the
# protocol between separately upgraded pieces. Before 1.0, a minor version
# (0.3.0) may change those, and its notes say how; a patch (0.2.1) never
# does. Commits marked breaking (`feat!:`) refuse a patch.
#
# Writes target/dist/: doppel-VERSION-macos-arm64.tar.gz, its .sha256,
# doppel.rb (the formula) and notes.md.
#
# Once per Mac: a "Developer ID Application" certificate in the keychain (an
# Apple Developer Program membership makes one), the notary credentials
# (`xcrun notarytool store-credentials doppel`), and `gh auth login`.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
repo="roushou/doppel"
tap="roushou/homebrew-doppel"
profile="${DOPPEL_NOTARY_PROFILE:-doppel}"

die() {
  echo "error: $*" >&2
  exit 1
}
step() { echo "release: $*" >&2; }

version=""
notes_file=""
package_only=no
while [[ $# -gt 0 ]]; do
  case "$1" in
    --package-only) package_only=yes ;;
    --notes)
      notes_file="${2:-}"
      shift
      [[ -f "$notes_file" ]] || die "--notes needs a file"
      ;;
    -*) die "unknown option $1" ;;
    *) version="$1" ;;
  esac
  shift
done

current_version() {
  sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' "$root/Cargo.toml"
}

subjects=""
if [[ "$package_only" == no ]]; then
  [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "usage: scripts/release.sh VERSION [--notes FILE] (VERSION like 0.2.1), or --package-only"
  tag="v$version"

  # Where releases come from: main as pushed, nothing else.
  [[ "$(git -C "$root" branch --show-current)" == main ]] || die "not on main"
  git -C "$root" diff --quiet HEAD || die "uncommitted changes: a release is built from a commit"
  git -C "$root" fetch -q origin main --tags
  [[ "$(git -C "$root" rev-parse HEAD)" == "$(git -C "$root" rev-parse origin/main)" ]] || die "main isn't origin/main: pull or push first"
  gh auth status >/dev/null 2>&1 || die "gh isn't signed in: gh auth login"
  xcrun notarytool history --keychain-profile "$profile" >/dev/null 2>&1 || die "no notary credentials in the keychain under \"$profile\": xcrun notarytool store-credentials $profile"

  # The last release before this one, and the commits since, without the
  # release commits themselves.
  last="$(git -C "$root" tag -l 'v[0-9]*' | sed 's/^v//' | grep -vx "$version" | sort -V | tail -1 || true)"
  range="${last:+v$last..}HEAD"
  subjects="$(git -C "$root" log --format=%s "$range" | grep -v "^build: release " || true)"
  if [[ "$(current_version)" != "$version" ]]; then
    [[ -z "$(git -C "$root" tag -l "$tag")" ]] || die "$tag exists already"
    if [[ -n "$last" ]]; then
      newest="$(printf '%s\n%s\n' "$last" "$version" | sort -V | tail -1)"
      [[ "$newest" == "$version" ]] || die "$version isn't after the last release, $last"
      [[ -n "$subjects" ]] || die "nothing has changed since $last"
      if [[ "${version%.*}" == "${last%.*}" ]] && grep -qE '^[a-z]+(\([^)]*\))?!:' <<<"$subjects"; then
        die "commits since $last are marked breaking: release a minor version, not $version"
      fi
    fi
    step "setting the version to $version"
    sed -i '' "/^\[workspace.package\]/,/^\[/s/^version = \".*\"/version = \"$version\"/" "$root/Cargo.toml"
    (cd "$root" && cargo update --workspace --offline -q)
    [[ "$(current_version)" == "$version" ]] || die "couldn't set the version in Cargo.toml"
    git -C "$root" commit -q -am "build: release $version"
    git -C "$root" push -q origin main
  else
    step "the version is $version already"
  fi
fi

version="$(current_version)"
name="doppel-$version-macos-arm64"
dist="$root/target/dist"
tag="v$version"

identity="${DOPPEL_SIGNING_IDENTITY:-}"
if [[ -z "$identity" ]]; then
  found="$(security find-identity -v -p codesigning | awk '/"Developer ID Application: /{print $2}')"
  if [[ -n "$found" && "$(wc -l <<<"$found")" -eq 1 ]]; then
    identity="$found"
  elif [[ "$package_only" == no ]]; then
    die "needs exactly one \"Developer ID Application\" identity in the keychain (or DOPPEL_SIGNING_IDENTITY)"
  fi
fi

published=no
if [[ "$package_only" == no ]] && gh release view "$tag" --repo "$repo" >/dev/null 2>&1; then
  published=yes
fi

if [[ "$published" == no ]]; then
  step "building $version"
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
  built="$("$dist/$name/doppel" --version)"
  [[ "$built" == "doppel $version" ]] || die "the build says $built, not doppel $version"

  if [[ "$package_only" == no ]]; then
    step "notarizing"
    # The notary service takes a zip, not a tarball; its ticket covers the
    # signed programs themselves, so the tarball of the same files passes
    # Gatekeeper too.
    (cd "$dist" && ditto -c -k --keepParent "$name" "$name.zip")
    xcrun notarytool submit "$dist/$name.zip" --keychain-profile "$profile" --wait
    rm "$dist/$name.zip"
  fi

  tar -C "$dist" -czf "$dist/$name.tar.gz" "$name"
  sha="$(shasum -a 256 "$dist/$name.tar.gz" | awk '{print $1}')"
  printf '%s  %s\n' "$sha" "$name.tar.gz" >"$dist/$name.tar.gz.sha256"
  rm -rf "${dist:?}/$name"
else
  step "$tag is published already"
  rm -rf "$dist"
  mkdir -p "$dist"
  gh release download "$tag" --repo "$repo" --dir "$dist" --pattern "$name.tar.gz*"
  sha="$(awk '{print $1}' "$dist/$name.tar.gz.sha256")"
fi

cat >"$dist/doppel.rb" <<EOF
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

if [[ "$package_only" == yes ]]; then
  echo "built $dist/$name.tar.gz (not notarized; signed: ${identity:-see above})"
  echo "formula: $dist/doppel.rb"
  exit 0
fi

# Notes: FILE's text, then the commits by kind, then how to install.
section() {
  local title="$1" pattern="$2" lines
  lines="$(grep -E "$pattern" <<<"$subjects" | sed -E 's/^[a-z]+(\(([^)]*)\))?!?: (.*)$/- **\2:** \3/; s/^- \*\*:\*\* /- /' || true)"
  if [[ -n "$lines" ]]; then
    printf '### %s\n\n%s\n\n' "$title" "$lines"
  fi
}
{
  if [[ -n "$notes_file" ]]; then
    cat "$notes_file"
    echo
  fi
  section "Breaking" '^[a-z]+(\([^)]*\))?!:'
  section "Features" '^feat(\([^)]*\))?:'
  section "Fixes" '^fix(\([^)]*\))?:'
  section "Other" '^(refactor|perf|build|docs|test|chore|ci|style)(\([^)]*\))?:'
  cat <<EOF
### Install

\`\`\`sh
brew install $repo/doppel   # or: brew upgrade doppel
doppel setup
\`\`\`

Signed with a Developer ID and notarized, for Apple silicon on macOS 27.
EOF
} >"$dist/notes.md"

if [[ "$published" == no ]]; then
  step "publishing $tag"
  gh release create "$tag" --repo "$repo" --target "$(git -C "$root" rev-parse HEAD)" \
    --title "Doppel $version" --notes-file "$dist/notes.md" \
    "$dist/$name.tar.gz" "$dist/$name.tar.gz.sha256"
fi

step "updating the tap"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
git clone -q "https://github.com/$tap.git" "$work/tap"
cp "$dist/doppel.rb" "$work/tap/Formula/doppel.rb"
if git -C "$work/tap" diff --quiet; then
  step "the tap has $version already"
else
  git -C "$work/tap" commit -q -am "doppel $version"
  git -C "$work/tap" push -q origin HEAD
fi

if command -v brew >/dev/null; then
  step "checking brew installs it"
  brew update -q >/dev/null
  if brew list "$repo/doppel" >/dev/null 2>&1; then
    brew upgrade -q "$repo/doppel" || true
  else
    brew install -q "$repo/doppel"
  fi
  installed="$("$(brew --prefix)/bin/doppel" --version)"
  [[ "$installed" == "doppel $version" ]] || die "brew installed $installed, not doppel $version"
  spctl -a -t install "$(brew --prefix)/opt/doppel/libexec/doppel" || die "Gatekeeper refuses the installed doppel"
fi

echo "released $tag: https://github.com/$repo/releases/tag/$tag"
