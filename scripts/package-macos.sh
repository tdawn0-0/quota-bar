#!/bin/sh
# Builds the macOS app the way a release needs it: NSH's defaults minus the
# permissions the app never asks for, plus the app icon, signed when an
# identity is given and notarized when a notarytool profile is.
#
#   scripts/package-macos.sh
#   CODESIGN_IDENTITY="Developer ID Application: … (TEAMID)" \
#   NOTARY_PROFILE=quota-bar scripts/package-macos.sh
set -eu

cd "$(dirname "$0")/.."

nsh package --target macos

APP=".nsh/out/Quota Bar.app"
PLIST="$APP/Contents/Info.plist"
RESOURCES="$APP/Contents/Resources"

# NSH declares the microphone and speech recognition for its native audio
# and dictation components. Quota Bar uses neither, and those descriptions
# make a security-minded user distrust an app that reads login files.
for key in NSMicrophoneUsageDescription NSSpeechRecognitionUsageDescription; do
    /usr/libexec/PlistBuddy -c "Delete :$key" "$PLIST" >/dev/null 2>&1 || true
done

# The icon Finder, Spotlight and Login Items show.
if [ -f assets/AppIcon.icns ]; then
    cp assets/AppIcon.icns "$RESOURCES/AppIcon.icns"
    /usr/libexec/PlistBuddy -c "Delete :CFBundleIconFile" "$PLIST" >/dev/null 2>&1 || true
    /usr/libexec/PlistBuddy -c "Add :CFBundleIconFile string AppIcon" "$PLIST"
fi

if [ -n "${CODESIGN_IDENTITY:-}" ]; then
    codesign --force --options runtime --timestamp \
        --sign "$CODESIGN_IDENTITY" "$APP"
fi

if [ -n "${NOTARY_PROFILE:-}" ]; then
    mkdir -p .nsh/out/notarize
    ditto -c -k --keepParent "$APP" .nsh/out/notarize/QuotaBar.zip
    xcrun notarytool submit .nsh/out/notarize/QuotaBar.zip \
        --keychain-profile "$NOTARY_PROFILE" --wait
    xcrun stapler staple "$APP"
fi

if [ -n "${DMG_PATH:-}" ]; then
    hdiutil create -volname "Quota Bar" -srcfolder "$APP" -ov -format UDZO "$DMG_PATH"
fi
