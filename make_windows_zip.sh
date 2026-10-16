#!/usr/bin/env bash
# Builds the Windows release zip from a Mac:
# dist/DexHowTo-Recorder-<version>-Windows-x64.zip
# with the app, a console build for troubleshooting, and a README.
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

rm -rf "$STAGING" "$DIR/dist/$NAME.zip"
mkdir -p "$STAGING"

echo "🔨 Building the Windows app..."
cargo build --release --target "$TARGET"
cp "$EXE" "$STAGING/DexHowTo Recorder.exe"

echo "🔨 Building the console version..."
cargo build --release --target "$TARGET" --features console
cp "$EXE" "$STAGING/DexHowTo Recorder (console).exe"

cp "$DIR/assets/README-windows.txt" "$STAGING/README.txt"

(cd "$DIR/dist" && zip -qr "$NAME.zip" "$NAME")
echo "✅ $DIR/dist/$NAME.zip"
