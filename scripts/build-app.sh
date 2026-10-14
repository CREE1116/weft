#!/bin/bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
cargo build --release --locked
swift build -c release --package-path "$ROOT/macos/SpeechEngine" --product weft-speech
mkdir -p "$ROOT/dist"
STAGING="$(mktemp -d "$ROOT/dist/.weft-build.XXXXXX")"
trap 'rm -rf "$STAGING"' EXIT
APP="$STAGING/Weft.app"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$ROOT/macos/Weft.swift" "$STAGING/main.swift"
MARKDOWN_SOURCES=()
while IFS= read -r -d '' source; do MARKDOWN_SOURCES+=("$source"); done < <(find "$ROOT/macos/Vendor/MarkdownEngine" -type f -name '*.swift' -print0)
swiftc -swift-version 5 -O -target "$(uname -m)-apple-macosx14.0" -framework Cocoa -framework Carbon -framework QuickLookUI -framework AVFoundation -framework SwiftUI \
  "$STAGING/main.swift" "$ROOT/macos/VoiceCapture.swift" "$ROOT/macos/Notes.swift" "$ROOT/macos/Onboarding.swift" "$ROOT/macos/IndexSettings.swift" "$ROOT/macos/QuickLinks.swift" "$ROOT/macos/Updates.swift" "$ROOT/macos/Brand.swift" "$ROOT/macos/WindowMotion.swift" "${MARKDOWN_SOURCES[@]}" -o "$APP/Contents/MacOS/Weft"
cp "$ROOT/macos/Info.plist" "$APP/Contents/Info.plist"
if [ -n "${WEFT_APP_VERSION:-}" ]; then
  VERSION="${WEFT_APP_VERSION#v}"
  if [[ ! "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then printf 'Invalid app release version\n' >&2; exit 1; fi
  /usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $VERSION" "$APP/Contents/Info.plist"
  /usr/libexec/PlistBuddy -c "Set :CFBundleVersion $VERSION" "$APP/Contents/Info.plist"
fi
# Replace the inode instead of overwriting a signed/running Mach-O in place.
# macOS can retain the old signature for an overwritten helper and kill it.
cp "$ROOT/target/release/weft" "$APP/Contents/Resources/weft"
cp "$ROOT/macos/SpeechEngine/.build/release/weft-speech" "$APP/Contents/Resources/weft-speech"
SIGN_IDENTITY="${WEFT_SIGN_IDENTITY:--}"
sign_bundle() {
  if [ "$SIGN_IDENTITY" = "-" ]; then
    codesign --force --sign - "$1"
  else
    codesign --force --sign "$SIGN_IDENTITY" --options runtime --timestamp "$1"
  fi
}
sign_bundle "$APP/Contents/Resources/weft"
sign_bundle "$APP/Contents/Resources/weft-speech"
cp "$ROOT/scripts/setup_media.sh" "$APP/Contents/Resources/setup-media.sh"
cp "$ROOT/scripts/setup-speech.sh" "$APP/Contents/Resources/setup-speech.sh"
cp -R "$ROOT/.agents/skills/weft-semantic-search" "$APP/Contents/Resources/weft-semantic-search"
mkdir -p "$STAGING/icon-generator"
cp "$ROOT/macos/MakeIcon.swift" "$STAGING/icon-generator/main.swift"
swiftc "$STAGING/icon-generator/main.swift" "$ROOT/macos/Brand.swift" -o "$STAGING/icon-generator/make-icon"
"$STAGING/icon-generator/make-icon" "$ROOT/dist/Weft.iconset"
iconutil -c icns "$ROOT/dist/Weft.iconset" -o "$APP/Contents/Resources/Weft.icns"
sign_bundle "$APP"
codesign --verify --deep --strict "$APP"
"$APP/Contents/Resources/weft" --version
DESTINATION="$ROOT/dist/Weft.app"
if [ -d "$DESTINATION" ]; then mv "$DESTINATION" "$STAGING/Previous.app"; fi
if ! mv "$APP" "$DESTINATION"; then
  if [ -d "$STAGING/Previous.app" ]; then mv "$STAGING/Previous.app" "$DESTINATION"; fi
  exit 1
fi
APP="$DESTINATION"
printf 'Built %s\n' "$APP"
