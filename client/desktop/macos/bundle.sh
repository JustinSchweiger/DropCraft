#!/bin/bash
# Baut DropCraft.app für macOS (ohne Apple-Entwicklerkonto, ad-hoc signiert).
#   bash client/desktop/macos/bundle.sh
# Ergebnis: client/target/release/DropCraft.app
set -euo pipefail
cd "$(dirname "$0")/../.."
cargo build --release -p dropcraft-desktop
OUT=target/release
APP=$OUT/DropCraft.app
rm -rf "$APP" "$OUT/AppIcon.iconset"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$OUT/DropCraft" "$APP/Contents/MacOS/DropCraft"
cp desktop/macos/Info.plist "$APP/Contents/Info.plist"
"$OUT/DropCraft" --export-iconset "$OUT/AppIcon.iconset"
iconutil -c icns "$OUT/AppIcon.iconset" -o "$APP/Contents/Resources/AppIcon.icns"
# Ad-hoc-Signatur: reicht zum Starten auf dem eigenen Mac
codesign --force --deep --sign - "$APP"
echo "Fertig: $(pwd)/$APP"
