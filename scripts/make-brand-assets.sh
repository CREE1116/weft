#!/bin/bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
STAGING="$(mktemp -d "${TMPDIR:-/tmp}/weft-brand.XXXXXX")"
trap 'rm -rf "$STAGING"' EXIT
# Use the app's drawing source so GitHub and the installed icon stay identical.
cp "$ROOT/macos/MakeIcon.swift" "$STAGING/main.swift"
swiftc "$STAGING/main.swift" "$ROOT/macos/Brand.swift" -o "$STAGING/make-icon"
"$STAGING/make-icon" "$STAGING/Weft.iconset"
cp "$STAGING/Weft.iconset/icon_512x512.png" "$ROOT/macos/assets/weft-logo.png"
printf 'Generated README logo from the app icon source\n'
