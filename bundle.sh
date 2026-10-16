#!/usr/bin/env bash
set -e

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$DIR"

echo "🔨 Building release binary (dexhowto_recorder)..."
cargo build --release

APP_NAME="DexHowTo Recorder.app"
CONTENTS="$DIR/$APP_NAME/Contents"
MACOS="$CONTENTS/MacOS"
RESOURCES="$CONTENTS/Resources"

echo "🧹 Cleaning previous app bundles..."
rm -rf "$DIR/$APP_NAME" "$DIR/Snagit Screen Recorder.app"

echo "📦 Creating macOS Application Bundle ($APP_NAME)..."
mkdir -p "$MACOS" "$RESOURCES"

cp "$DIR/target/release/dexhowto_recorder" "$MACOS/dexhowto_recorder"
chmod +x "$MACOS/dexhowto_recorder"

# App icon (regenerate with: swift assets/make_icon.swift assets/AppIcon-1024.png)
cp "$DIR/assets/AppIcon.icns" "$RESOURCES/AppIcon.icns"

cat << 'EOF' > "$CONTENTS/Info.plist"
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key>
    <string>en</string>
    <key>CFBundleIconFile</key>
    <string>AppIcon</string>
    <key>CFBundleExecutable</key>
    <string>dexhowto_recorder</string>
    <key>CFBundleIdentifier</key>
    <string>com.dexhowto.recorder</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleName</key>
    <string>DexHowTo Recorder</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>0.1.0</string>
    <key>CFBundleVersion</key>
    <string>1</string>
    <key>LSMinimumSystemVersion</key>
    <string>14.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSScreenCaptureUsageDescription</key>
    <string>DexHowTo Recorder needs access to your screen to record video.</string>
    <key>NSCameraUsageDescription</key>
    <string>DexHowTo Recorder needs access to your camera to record your face alongside the screen.</string>
    <key>NSMicrophoneUsageDescription</key>
    <string>DexHowTo Recorder needs access to your microphone to record audio commentary.</string>
</dict>
</plist>
EOF

# macOS privacy permissions (Screen Recording, Microphone) are tied to the app's
# designated requirement. A plain ad-hoc signature uses the binary's hash, which
# changes on every build, so macOS forgets the grant and System Settings shows a
# stale entry. Pin the requirement to the bundle identifier so it survives rebuilds.
# Set DEXHOWTO_SIGN_IDENTITY to a code-signing certificate name to sign with it instead.
SIGN_IDENTITY="${DEXHOWTO_SIGN_IDENTITY:--}"
BUNDLE_ID="com.dexhowto.recorder"

echo "🔏 Code signing app bundle (identity: $SIGN_IDENTITY)..."
codesign --force --deep --sign "$SIGN_IDENTITY" \
    --identifier "$BUNDLE_ID" \
    --requirements "=designated => identifier \"$BUNDLE_ID\"" \
    "$DIR/$APP_NAME"

echo "📦 Installing to /Applications/$APP_NAME..."
rm -rf "/Applications/$APP_NAME"
# ditto keeps the signature intact, so no re-signing is needed
ditto "$DIR/$APP_NAME" "/Applications/$APP_NAME"
/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -f "/Applications/$APP_NAME" 2>/dev/null || true

echo "✅ App bundle created & installed to /Applications/$APP_NAME"
echo "👉 You can open it by running: open '/Applications/$APP_NAME'"
