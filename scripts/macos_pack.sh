#!/usr/bin/env bash
set -euo pipefail

APP_NAME="Ripor"
BIN_UI="agent-ui-macos"
BIN_AGENT="agent-daemon"
BIN_HELPER="agent-login-macos"
ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
TARGET="release"
DIST_DIR="$ROOT_DIR/dist"
BUNDLE_DIR="$DIST_DIR/${APP_NAME}.app"

echo "[pack] building release binaries…"
cargo build -q -p "$BIN_UI" --$TARGET
cargo build -q -p "$BIN_AGENT" --$TARGET
cargo build -q -p "$BIN_HELPER" --$TARGET

echo "[pack] assembling bundle at $BUNDLE_DIR"
rm -rf "$BUNDLE_DIR"
mkdir -p "$BUNDLE_DIR/Contents/MacOS"
mkdir -p "$BUNDLE_DIR/Contents/Resources/bin"
mkdir -p "$BUNDLE_DIR/Contents/Resources"
mkdir -p "$BUNDLE_DIR/Contents/Library/LoginItems/RiporHelper.app/Contents/MacOS"
mkdir -p "$BUNDLE_DIR/Contents/Library/LoginItems/RiporHelper.app/Contents/Resources"

cp "$ROOT_DIR/target/$TARGET/$BIN_UI" "$BUNDLE_DIR/Contents/MacOS/RiporUI"
cp "$ROOT_DIR/target/$TARGET/$BIN_AGENT" "$BUNDLE_DIR/Contents/Resources/bin/agent-daemon"
cp "$ROOT_DIR/target/$TARGET/$BIN_HELPER" "$BUNDLE_DIR/Contents/Library/LoginItems/RiporHelper.app/Contents/MacOS/RiporHelper"

# Optional icon for status bar
if [ -f "$ROOT_DIR/assets/icons/macos/iconTemplate.png" ]; then
  cp "$ROOT_DIR/assets/icons/macos/iconTemplate.png" "$BUNDLE_DIR/Contents/Resources/iconTemplate.png"
fi

APP_VERSION="${APP_VERSION:-0.1.0}"
BUNDLE_ID="${BUNDLE_ID:-com.ripor.Ripor}"
HELPER_BUNDLE_ID="${HELPER_BUNDLE_ID:-com.ripor.Ripor.LoginItem}"

cat > "$BUNDLE_DIR/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key><string>en</string>
  <key>CFBundleExecutable</key><string>RiporUI</string>
  <key>CFBundleIdentifier</key><string>${BUNDLE_ID}</string>
  <key>CFBundleName</key><string>Ripor</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleVersion</key><string>${APP_VERSION}</string>
  <key>CFBundleShortVersionString</key><string>${APP_VERSION}</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>LSUIElement</key><true/>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

# Helper Info.plist
cat > "$BUNDLE_DIR/Contents/Library/LoginItems/RiporHelper.app/Contents/Info.plist" <<HPLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key><string>en</string>
  <key>CFBundleExecutable</key><string>RiporHelper</string>
  <key>CFBundleIdentifier</key><string>${HELPER_BUNDLE_ID}</string>
  <key>CFBundleName</key><string>RiporHelper</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleVersion</key><string>${APP_VERSION}</string>
  <key>CFBundleShortVersionString</key><string>${APP_VERSION}</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>LSUIElement</key><true/>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
HPLIST

echo "[pack] done: $BUNDLE_DIR"
echo "Open with: open '$BUNDLE_DIR'"
