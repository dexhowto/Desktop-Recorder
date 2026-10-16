#!/usr/bin/env bash
# Builds a DMG for release: dist/DexHowTo-Recorder-<version>.dmg
# The DMG holds the app and an Applications shortcut, so people install it by
# dragging the app onto the shortcut.
set -e

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$DIR"

APP_NAME="DexHowTo Recorder.app"
VOLUME_NAME="DexHowTo Recorder"

# Build and sign the app bundle (bundle.sh also installs it to /Applications)
./bundle.sh

VERSION="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$DIR/$APP_NAME/Contents/Info.plist")"
DMG="$DIR/dist/DexHowTo-Recorder-$VERSION.dmg"

STAGING="$(mktemp -d)"
trap 'rm -rf "$STAGING"' EXIT

echo "📀 Creating $DMG..."
ditto "$DIR/$APP_NAME" "$STAGING/$APP_NAME"
ln -s /Applications "$STAGING/Applications"

mkdir -p "$DIR/dist"
rm -f "$DMG"
hdiutil create -volname "$VOLUME_NAME" -srcfolder "$STAGING" -fs HFS+ -format UDZO -ov "$DMG" >/dev/null

# Sign the DMG with the same identity as the app (ad-hoc by default)
codesign --force --sign "${DEXHOWTO_SIGN_IDENTITY:--}" "$DMG"

echo "✅ $DMG ($(du -h "$DMG" | cut -f1))"
