#!/usr/bin/env bash
# Package a Linux release build as .tar.gz, .deb, .rpm and AppImage.
# Usage: package-linux.sh <version> <rust target> <name, e.g. linux_amd64> <out dir>
# Needs cargo-deb and cargo-generate-rpm; downloads appimagetool.
set -euo pipefail
version="$1"
target="$2"
name="$3"
out="$(mkdir -p "$4" && cd "$4" && pwd)"
cd "$(dirname "$0")/../.."
bin="target/$target/release/headway"

tar -czf "$out/headway_${version}_${name}.tar.gz" -C "$(dirname "$bin")" headway
cargo deb --no-build --target "$target" --output "$out/"
cargo generate-rpm --target "$target" --output "$out/"

# AppImage: an AppDir with the binary, desktop entry and icon, packed by appimagetool.
case "$target" in
    x86_64-*) arch=x86_64 ;;
    aarch64-*) arch=aarch64 ;;
    *) echo "no AppImage for $target" >&2; exit 1 ;;
esac
work="$(mktemp -d)"
appdir="$work/Headway.AppDir"
install -Dm755 "$bin" "$appdir/usr/bin/headway"
install -Dm644 assets/linux/headway.desktop "$appdir/usr/share/applications/headway.desktop"
install -Dm644 assets/icon/headway-256.png "$appdir/usr/share/icons/hicolor/256x256/apps/headway.png"
cp assets/linux/headway.desktop "$appdir/headway.desktop"
cp assets/icon/headway-256.png "$appdir/headway.png"
ln -s usr/bin/headway "$appdir/AppRun"
curl -fsSL -o "$work/appimagetool" \
    "https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-$arch.AppImage"
chmod +x "$work/appimagetool"
# Runners have no FUSE, so let the tool unpack itself instead of mounting.
ARCH="$arch" APPIMAGE_EXTRACT_AND_RUN=1 "$work/appimagetool" --no-appstream "$appdir" \
    "$out/headway_${version}_${name}.AppImage"
