#!/bin/bash
# Build on macOS. This never modifies release/windows.
set -euo pipefail

build_mode="${1:---beta}"
if [[ $# -gt 1 || ( "$build_mode" != "--beta" && "$build_mode" != "--release" ) ]]; then
  echo "Usage: bash scripts/build-macos.sh [--beta|--release]" >&2
  exit 1
fi

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "Run this script on macOS with Xcode Command Line Tools and Rust installed." >&2
  exit 1
fi

project_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$project_root"
output="$project_root/release/macos"
mkdir -p "$output"
# Serialize version allocation and packaging; never overwrite existing artifacts.
build_lock="$output/.build-lock"
if ! mkdir "$build_lock" 2>/dev/null; then
  echo "Another macOS build is active ($build_lock)." >&2
  exit 1
fi
trap 'rmdir "$build_lock"' EXIT
if [[ "$build_mode" == "--release" ]]; then
  build_version="$(cat "$project_root/packaging/macos/release-version.txt")"
  if [[ ! "$build_version" =~ ^[0-9]+\.[0-9]+(\.[0-9]+)?$ ]]; then
    echo "Invalid release version: $build_version" >&2
    exit 1
  fi
  release_name="Little Mungbean-v$build_version"
  bundle_name="Little Mungbean"
  for artifact in "$output/$bundle_name.app" "$output/$release_name-macos-arm64.zip" "$output/$release_name-macos-arm64.dmg"; do
    if [[ -e "$artifact" ]]; then
      echo "Refusing to overwrite existing release artifact: $artifact" >&2
      exit 1
    fi
  done
else
  version_file="$project_root/packaging/macos/beta-version.txt"
  previous_version="$(cat "$version_file")"
  if [[ ! "$previous_version" =~ ^1\.1\.[0-9]+$ ]]; then
    echo "Invalid beta version: $previous_version" >&2
    exit 1
  fi
  build_number="${previous_version##*.}"
  while :; do
    build_number=$((10#$build_number + 1))
    build_version="1.1.$build_number"
    release_name="Little Mungbean-Beta-v$build_version"
    if [[ ! -e "$output/$release_name.app" && ! -e "$output/$release_name-macos-arm64.zip" ]]; then
      break
    fi
  done
  bundle_name="$release_name"
  # Reserve before compiling: failed beta attempts consume a number.
  printf '%s\n' "$build_version" > "$version_file.tmp"
  mv "$version_file.tmp" "$version_file"
fi
export LITTLE_MUNGBEAN_MACOS_VERSION="$build_version"
echo "Building $release_name"
target="aarch64-apple-darwin"
export MACOSX_DEPLOYMENT_TARGET=13.0

if command -v rustup >/dev/null 2>&1; then
  if ! rustup target list --installed | /usr/bin/grep -qx "$target"; then
    rustup target add "$target"
  fi
fi
cargo build --locked --release --target "$target"

bundle="$output/$bundle_name.app"
mkdir -p "$bundle/Contents/MacOS" "$bundle/Contents/Resources"
cp "target/$target/release/little-mungbean" "$bundle/Contents/MacOS/Little-Mungbean"
cp assets/gui/macos-logo/macos.icns "$bundle/Contents/Resources/Little-Mungbean.icns"
chmod 755 "$bundle/Contents/MacOS/Little-Mungbean"
cp packaging/macos/Info.plist "$bundle/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $build_version" "$bundle/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleVersion $build_version" "$bundle/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleDisplayName 'Little Mungbean'" "$bundle/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleName 'Little Mungbean'" "$bundle/Contents/Info.plist"
printf 'APPL????' > "$bundle/Contents/PkgInfo"
/usr/bin/plutil -lint "$bundle/Contents/Info.plist"

if [[ -n "${LITTLE_MUNGBEAN_MAC_SIGN_IDENTITY:-}" ]]; then
  /usr/bin/codesign --force --options runtime --timestamp \
    --entitlements packaging/macos/Entitlements.plist \
    --sign "$LITTLE_MUNGBEAN_MAC_SIGN_IDENTITY" "$bundle"
else
  # Ad-hoc signature supports local Apple Silicon testing, not notarized distribution.
  /usr/bin/codesign --force --sign - "$bundle"
fi
/usr/bin/codesign --verify --deep --strict "$bundle"

archive="$output/$release_name-macos-arm64.zip"
/usr/bin/ditto -c -k --sequesterRsrc --keepParent "$bundle" "$archive"
/usr/bin/shasum -a 256 "$archive" > "$archive.sha256"
echo "Built: $bundle"
echo "Archive: $archive"
if [[ "$build_mode" == "--release" ]]; then
  bash scripts/package-macos-dmg.sh "$bundle" "$output/$release_name-macos-arm64.dmg" "$build_version"
fi
