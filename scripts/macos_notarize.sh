#!/usr/bin/env bash
set -euo pipefail

APP_PATH=${1:-"dist/Ripor.app"}
ZIP_PATH=${2:-"dist/Ripor.zip"}
NOTARY_PROFILE=${NOTARY_PROFILE:-"NotaryProfile"}

if [ ! -d "$APP_PATH" ]; then echo "[notarize] App not found: $APP_PATH"; exit 1; fi

echo "[notarize] Zipping $APP_PATH -> $ZIP_PATH"
mkdir -p "$(dirname "$ZIP_PATH")"
ditto -c -k --keepParent "$APP_PATH" "$ZIP_PATH"

echo "[notarize] Submitting to notarytool with profile $NOTARY_PROFILE"
xcrun notarytool submit "$ZIP_PATH" --keychain-profile "$NOTARY_PROFILE" --wait

echo "[notarize] Stapling ticket"
xcrun stapler staple "$APP_PATH"

echo "[notarize] Verifying spctl"
spctl --assess --type execute -v "$APP_PATH" || true
echo "[notarize] Done"

