#!/usr/bin/env sh
set -eu

prefix="${PREFIX:-$HOME/.local}"
bindir="$prefix/bin"
icondir="$prefix/share/icons/hicolor/256x256/apps"
appdir="$prefix/share/applications"

install -Dm755 target/release/dde3-toolkit "$bindir/dde3-toolkit"
install -Dm644 assets/icon.png "$icondir/dde3-toolkit.png"
install -Dm644 assets/dde3-toolkit.desktop "$appdir/dde3-toolkit.desktop"

if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$appdir" || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -f -t "$prefix/share/icons/hicolor" || true
fi

echo "Installed DDE3 Toolkit to $prefix"
