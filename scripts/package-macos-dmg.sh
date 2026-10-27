#!/bin/bash
# Package an already signed macOS app without modifying the app or installing it.
set -euo pipefail

if [[ $# -ne 3 || "$(uname -s)" != "Darwin" ]]; then
  echo "Usage: bash scripts/package-macos-dmg.sh <app> <output.dmg> <version> (macOS only)" >&2
  exit 1
fi
bundle="$1"
disk_image="$2"
version="$3"
if [[ ! -d "$bundle/Contents/MacOS" || ! "$version" =~ ^[0-9]+\.[0-9]+(\.[0-9]+)?$ ]]; then
  echo "Invalid application bundle or version." >&2
  exit 1
fi
if [[ -e "$disk_image" ]]; then
  echo "Refusing to overwrite an existing disk image: $disk_image" >&2
  exit 1
fi
/usr/bin/codesign --verify --deep --strict "$bundle"
output_dir="$(dirname "$disk_image")"
mkdir -p "$output_dir"
dmg_stage="$(mktemp -d "$output_dir/.dmg-stage.XXXXXX")"
trap 'rm -rf "$dmg_stage"' EXIT
/usr/bin/ditto "$bundle" "$dmg_stage/$(basename "$bundle")"
ln -s /Applications "$dmg_stage/Applications"
/usr/bin/hdiutil create -volname "Little Mungbean $version" -srcfolder "$dmg_stage" \
  -fs HFS+ -format UDZO "$disk_image"
/usr/bin/hdiutil verify "$disk_image"
/usr/bin/shasum -a 256 "$disk_image" > "$disk_image.sha256"
echo "Disk image: $disk_image"
