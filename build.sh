#!/bin/sh
# Build Kobo Shelf.app and install it to ~/Applications.
set -e
cd "$(dirname "$0")"
APP="$HOME/Applications/Kobo Shelf.app"
cargo build --release
rm -rf build && mkdir -p "build/Kobo Shelf.app/Contents/MacOS" "build/Kobo Shelf.app/Contents/Resources"
cp target/release/kobo-shelf "build/Kobo Shelf.app/Contents/MacOS/"
cp Info.plist "build/Kobo Shelf.app/Contents/"
cp icon/AppIcon.icns "build/Kobo Shelf.app/Contents/Resources/"
codesign --force --sign - "build/Kobo Shelf.app"
pkill -x kobo-shelf || true
rm -rf "$APP"
mv "build/Kobo Shelf.app" "$APP"
rmdir build
echo "Installed $APP"
