#!/usr/bin/env bash
# Build a universal Headway.app from the Apple silicon and Intel binaries, and a .dmg
# with it and a shortcut to /Applications.
# Usage: package-macos.sh <version> <arm64 binary> <x86_64 binary> <out dir>
set -euo pipefail
version="$1"
arm="$2"
intel="$3"
out="$(mkdir -p "$4" && cd "$4" && pwd)"
root="$(cd "$(dirname "$0")/../.." && pwd)"

work="$(mktemp -d)"
app="$work/dmg/Headway.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
lipo -create "$arm" "$intel" -output "$app/Contents/MacOS/headway"
cp "$root/assets/icon/headway.icns" "$app/Contents/Resources/headway.icns"
sed "s/@VERSION@/$version/g" "$root/assets/macos/Info.plist" > "$app/Contents/Info.plist"
plutil -lint "$app/Contents/Info.plist"

# Ad-hoc signature: required to run on Apple silicon. Replace with a Developer ID
# signature (and notarization) to get past Gatekeeper without a warning.
codesign --force --sign - "$app"

ln -s /Applications "$work/dmg/Applications"
hdiutil create -volname "Headway" -srcfolder "$work/dmg" -ov -format UDZO \
    "$out/headway_${version}_macos_universal.dmg"
