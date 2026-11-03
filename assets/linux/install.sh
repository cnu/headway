#!/usr/bin/env bash
# Install Headway's desktop entry and icons for the current user, so launchers
# and Wayland compositors show the Headway icon (matched via the "headway" app id).
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
icons="$here/../icon"
share="${XDG_DATA_HOME:-$HOME/.local/share}"

for size in 16 32 48 64 128 256 512; do
    install -Dm644 "$icons/headway-$size.png" "$share/icons/hicolor/${size}x${size}/apps/headway.png"
done
install -Dm644 "$icons/headway.svg" "$share/icons/hicolor/scalable/apps/headway.svg"

# Point Exec at a given binary (default: the release build next to this repo).
bin="${1:-$(cd "$here/../.." && pwd)/target/release/headway}"
install -d "$share/applications"
sed "s|^Exec=.*|Exec=$bin|" "$here/headway.desktop" > "$share/applications/headway.desktop"

gtk-update-icon-cache -q -t "$share/icons/hicolor" 2>/dev/null || true
update-desktop-database -q "$share/applications" 2>/dev/null || true
echo "Installed Headway desktop entry (Exec=$bin)"
