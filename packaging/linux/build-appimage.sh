#!/usr/bin/env bash
# Builds gitem's size-optimised binary and wraps it in an AppImage, leaving it in target/dist. Needs
# appimagetool on PATH, or named by APPIMAGETOOL. It installs nothing and doesn't run what it makes.
#
# Git, GTK and the graphics libraries come from the machine it runs on, as they do for the plain
# archive. So build on an old distribution: the result only starts on systems with this glibc or newer.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
here="$root/packaging/linux"

version="$(sed -n 's/^version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$root/Cargo.toml" | head -n 1)"
[ -n "$version" ] || { echo "No version in Cargo.toml" >&2; exit 1; }

if [ "${1:-}" != "--skip-build" ]; then
    cargo build --manifest-path "$root/Cargo.toml" --profile dist -p gitem
fi
binary="$root/target/dist/gitem"
[ -f "$binary" ] || { echo "No binary at $binary" >&2; exit 1; }

tool="${APPIMAGETOOL:-appimagetool}"
command -v "$tool" >/dev/null || { echo "$tool isn't on PATH. Set APPIMAGETOOL to where it is." >&2; exit 1; }

appdir="$root/target/dist/gitem.AppDir"
rm -rf "$appdir"
mkdir -p "$appdir/usr/bin"
install -m 755 "$binary" "$appdir/usr/bin/gitem"
install -m 755 "$here/AppRun" "$appdir/AppRun"
install -m 644 "$here/gitem.desktop" "$appdir/gitem.desktop"
install -m 644 "$here/gitem.png" "$appdir/gitem.png"
# What file managers look for, beside the name appimagetool wants.
ln -s gitem.png "$appdir/.DirIcon"

ARCH="${ARCH:-x86_64}" "$tool" "$appdir" "$root/target/dist/gitem-$version-${ARCH:-x86_64}.AppImage"
