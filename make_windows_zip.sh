#!/usr/bin/env bash
# Builds the Windows release zip from a Mac:
# dist/DexHowTo-Recorder-<version>-Windows-x64.zip
# with the app, a console build for troubleshooting, ffmpeg.exe and ffprobe.exe
# (Gyan's GPL "essentials" build, pinned and checked by SHA-256), and a README.
# Needs: brew install mingw-w64 && rustup target add x86_64-pc-windows-gnu
set -e

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$DIR"

TARGET=x86_64-pc-windows-gnu
export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc
VERSION="$(grep -m1 '^version' Cargo.toml | cut -d '"' -f2)"
NAME="DexHowTo-Recorder-$VERSION-Windows-x64"
STAGING="$DIR/dist/$NAME"
EXE="$DIR/target/$TARGET/release/dexhowto_recorder.exe"

FFMPEG_VERSION="9.0.2"
FFMPEG_ZIP="ffmpeg-$FFMPEG_VERSION-essentials_build.zip"
FFMPEG_URL="https://github.com/GyanD/codexffmpeg/releases/download/$FFMPEG_VERSION/$FFMPEG_ZIP"
FFMPEG_SHA256="60f467265b1e312373dbcd92200c2618a74850f98d3d078e94296bb3fa2047ba"
FFMPEG_CACHE="$DIR/target/ffmpeg-windows"

rm -rf "$STAGING" "$DIR/dist/$NAME.zip"
mkdir -p "$STAGING"

echo "🔨 Building the Windows app..."
cargo build --release --target "$TARGET"
cp "$EXE" "$STAGING/DexHowTo Recorder.exe"

echo "🔨 Building the console version..."
cargo build --release --target "$TARGET" --features console
cp "$EXE" "$STAGING/DexHowTo Recorder (console).exe"

cp "$DIR/assets/README-windows.txt" "$STAGING/README.txt"

echo "📦 Adding ffmpeg $FFMPEG_VERSION..."
mkdir -p "$FFMPEG_CACHE"
if [ ! -f "$FFMPEG_CACHE/$FFMPEG_ZIP" ]; then
    curl -fL --progress-bar -o "$FFMPEG_CACHE/$FFMPEG_ZIP.part" "$FFMPEG_URL"
    mv "$FFMPEG_CACHE/$FFMPEG_ZIP.part" "$FFMPEG_CACHE/$FFMPEG_ZIP"
fi
if ! echo "$FFMPEG_SHA256  $FFMPEG_CACHE/$FFMPEG_ZIP" | shasum -a 256 -c - >/dev/null; then
    echo "❌ $FFMPEG_ZIP does not match its SHA-256; delete it from $FFMPEG_CACHE and retry"
    exit 1
fi
FFMPEG_DIR="ffmpeg-$FFMPEG_VERSION-essentials_build"
unzip -q -j -o "$FFMPEG_CACHE/$FFMPEG_ZIP" "$FFMPEG_DIR/bin/ffmpeg.exe" "$FFMPEG_DIR/bin/ffprobe.exe" -d "$STAGING"
# ffmpeg is GPL-3.0: ship its license and build notes, and point to its source
mkdir -p "$STAGING/ffmpeg-license"
unzip -q -j -o "$FFMPEG_CACHE/$FFMPEG_ZIP" "$FFMPEG_DIR/LICENSE" "$FFMPEG_DIR/README.txt" -d "$STAGING/ffmpeg-license"
cat > "$STAGING/ffmpeg-license/SOURCE.txt" <<SRC
ffmpeg.exe and ffprobe.exe are FFmpeg $FFMPEG_VERSION, "essentials" build by Gyan Doshi,
licensed under the GNU GPL version 3 (see LICENSE in this folder).

Build: $FFMPEG_URL
FFmpeg source code: https://ffmpeg.org/releases/ffmpeg-$FFMPEG_VERSION.tar.xz
Build details and the sources of the bundled libraries: https://www.gyan.dev/ffmpeg/builds/
SRC

(cd "$DIR/dist" && zip -qr "$NAME.zip" "$NAME")
echo "✅ $DIR/dist/$NAME.zip"
