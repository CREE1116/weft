#!/bin/bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
"$ROOT/scripts/build-app.sh"
STAGING="$(mktemp -d "$ROOT/dist/.weft-dmg.XXXXXX")"
trap 'rm -rf "$STAGING"' EXIT
cp -R "$ROOT/dist/Weft.app" "$STAGING/Weft.app"
ln -s /Applications "$STAGING/Applications"
VERSION=$(/usr/libexec/PlistBuddy -c 'Print CFBundleShortVersionString' "$ROOT/dist/Weft.app/Contents/Info.plist")
DMG="$ROOT/dist/Weft-$VERSION-$(uname -m).dmg"
hdiutil create -volname Weft -srcfolder "$STAGING" -ov -format UDZO "$DMG"
if [ -n "${WEFT_NOTARY_PROFILE:-}" ]; then
  xcrun notarytool submit "$DMG" --keychain-profile "$WEFT_NOTARY_PROFILE" --wait
  xcrun stapler staple "$ROOT/dist/Weft.app"
  xcrun stapler staple "$DMG"
fi
printf 'Built %s\n' "$DMG"
/usr/bin/ditto -c -k --keepParent "$ROOT/dist/Weft.app" "$ROOT/dist/Weft-$VERSION-$(uname -m).zip"
printf 'Built update archive %s\n' "$ROOT/dist/Weft-$VERSION-$(uname -m).zip"
