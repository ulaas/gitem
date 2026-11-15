#!/usr/bin/env bash
# Builds gitem for Apple silicon, wraps it as gitem.app, signs it and puts it in a disk image, leaving
# target/dist/gitem-<version>-macos-arm64.dmg. Needs the Xcode command line tools and Rust's
# aarch64-apple-darwin target (added through rustup if it's there). It installs nothing and doesn't
# run what it makes.
#
# Signing is ad hoc unless MACOS_SIGN_IDENTITY names a certificate in the keychain, such as
# "Developer ID Application: Name (TEAMID)". With one, MACOS_NOTARY_PROFILE can name a profile
# saved by `xcrun notarytool store-credentials`, and the image is then notarized and stapled.
#
#   packaging/macos/build-dmg.sh [--skip-build]
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
here="$root/packaging/macos"
target=aarch64-apple-darwin

[ "$(uname -s)" = Darwin ] || { echo "This builds on macOS only." >&2; exit 1; }

version="$(sed -n 's/^version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$root/Cargo.toml" | head -n 1)"
[ -n "$version" ] || { echo "No version in Cargo.toml" >&2; exit 1; }

identity="${MACOS_SIGN_IDENTITY:--}"
dist="$root/target/dist"
work="$dist/macos"
app="$work/gitem.app"
dmg="$dist/gitem-$version-macos-arm64.dmg"

if [ "${1:-}" != "--skip-build" ]; then
    if command -v rustup >/dev/null && ! rustup target list --installed | grep -qx "$target"; then
        rustup target add "$target"
    fi
    # The oldest macOS that Rust builds for Apple silicon, and what Info.plist says.
    MACOSX_DEPLOYMENT_TARGET=11.0 cargo build --manifest-path "$root/Cargo.toml" --locked --profile dist -p gitem --target "$target"
fi
binary="$root/target/$target/dist/gitem"
[ -f "$binary" ] || { echo "No binary at $binary" >&2; exit 1; }
archs="$(lipo -archs "$binary")"
[ "$archs" = arm64 ] || { echo "$binary is for $archs, not arm64 alone" >&2; exit 1; }

rm -rf "$work"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
install -m 755 "$binary" "$app/Contents/MacOS/gitem"
sed "s/@VERSION@/$version/g" "$here/Info.plist" > "$app/Contents/Info.plist"
printf 'APPL????' > "$app/Contents/PkgInfo"
cp "$root/README.md" "$root/LICENSE" "$root/THIRD-PARTY-NOTICES.md" "$app/Contents/Resources/"
plutil -lint "$app/Contents/Info.plist"

# The icon, from the logo's own geometry: one large picture, scaled by sips to every size.
iconset="$work/gitem.iconset"
mkdir -p "$iconset"
rustc -O --edition 2021 -o "$work/icon" "$here/icon.rs"
"$work/icon" "$work/icon-1024.png" 1024
for size in 16 32 128 256 512; do
    sips -z "$size" "$size" "$work/icon-1024.png" --out "$iconset/icon_${size}x${size}.png" >/dev/null
    double=$((size * 2))
    sips -z "$double" "$double" "$work/icon-1024.png" --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/gitem.icns"

# Anything nested is signed before what holds it. There's nothing nested today; this is for later.
sign() {
    local flags=(--force --options runtime --sign "$identity")
    if [ "$identity" = - ]; then flags+=(--timestamp=none); else flags+=(--timestamp); fi
    codesign "${flags[@]}" "$@"
}
find "$app/Contents" \( -name '*.dylib' -o -name '*.framework' \) -print0 2>/dev/null | while IFS= read -r -d '' nested; do
    sign "$nested"
done
sign --entitlements "$here/gitem.entitlements" "$app/Contents/MacOS/gitem"
sign --deep --entitlements "$here/gitem.entitlements" "$app"
codesign --verify --deep --strict --verbose=2 "$app"

# The image: the app, and a link to Applications to drag it onto.
stage="$work/dmg"
mkdir -p "$stage"
cp -R "$app" "$stage/"
ln -s /Applications "$stage/Applications"
cp "$app/Contents/Resources/gitem.icns" "$stage/.VolumeIcon.icns"
rw="$work/rw.dmg"
hdiutil create -volname "gitem $version" -srcfolder "$stage" -fs HFS+ -format UDRW -ov "$rw" >/dev/null
# The volume shows the app's icon. Its root is only marked for that once it's mounted.
if command -v SetFile >/dev/null; then
    mount="$work/mnt"
    mkdir -p "$mount"
    hdiutil attach "$rw" -nobrowse -noverify -noautoopen -mountpoint "$mount" >/dev/null
    SetFile -a C "$mount" || true
    hdiutil detach "$mount" -quiet || hdiutil detach "$mount" -force -quiet
fi
rm -f "$dmg"
hdiutil convert "$rw" -format "${DMG_FORMAT:-ULFO}" -o "$dmg" >/dev/null
rm -f "$rw"

if [ "$identity" != - ]; then
    sign "$dmg"
    if [ -n "${MACOS_NOTARY_PROFILE:-}" ]; then
        xcrun notarytool submit "$dmg" --keychain-profile "$MACOS_NOTARY_PROFILE" --wait
        xcrun stapler staple "$dmg"
    fi
fi

hdiutil verify "$dmg" >/dev/null
echo "$dmg"
shasum -a 256 "$dmg"
