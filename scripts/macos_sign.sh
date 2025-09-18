#!/usr/bin/env bash
set -euo pipefail

APP_PATH=${1:-"dist/Ripor.app"}
CERT_NAME=${CERT_NAME:-"Developer ID Application: YOUR NAME (TEAMID)"}
TEAM_ID=${TEAM_ID:-"TEAMID"}
ENT_MAIN=${ENT_MAIN:-"assets/macos/Entitlements.plist"}
ENT_HELPER=${ENT_HELPER:-"assets/macos/HelperEntitlements.plist"}

if [ ! -d "$APP_PATH" ]; then echo "[sign] App not found: $APP_PATH"; exit 1; fi

echo "[sign] Using certificate: $CERT_NAME"
echo "[sign] Entitlements main: $ENT_MAIN"
echo "[sign] Entitlements helper: $ENT_HELPER"

# Paths
BIN_DAEMON="$APP_PATH/Contents/Resources/bin/agent-daemon"
HELPER_APP="$APP_PATH/Contents/Library/LoginItems/RiporHelper.app"
HELPER_BIN="$HELPER_APP/Contents/MacOS/RiporHelper"
UI_BIN="$APP_PATH/Contents/MacOS/RiporUI"

function sign() {
  local target="$1" ent="$2"
  echo "[sign] $target"
  codesign --force --timestamp --options runtime -s "$CERT_NAME" ${ent:+--entitlements "$ent"} "$target"
}

# 1) agent-daemon
[ -f "$BIN_DAEMON" ] && sign "$BIN_DAEMON" "$ENT_MAIN"

# 2) helper bin + helper app
if [ -d "$HELPER_APP" ]; then
  [ -f "$HELPER_BIN" ] && sign "$HELPER_BIN" "$ENT_HELPER"
  sign "$HELPER_APP" "$ENT_HELPER"
fi

# 3) UI bin y app
[ -f "$UI_BIN" ] && sign "$UI_BIN" "$ENT_MAIN"
sign "$APP_PATH" "$ENT_MAIN"

echo "[sign] Done"

