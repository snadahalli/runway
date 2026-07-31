#!/bin/bash
# Build Runway.app — menu bar app and Notification Centre widget.
#
#   ./build.sh                              ad-hoc signed; no Apple account needed
#   ./build.sh --install                    ...and install to /Applications, then launch
#   SIGN_IDENTITY="Runway Dev" ./build.sh   stable self-signed identity (see README)
#   DEVELOPMENT_TEAM=ABCDE12345 ./build.sh  Xcode-managed signing, if you have a team
#
# Why the manual codesign pass:
#
# The widget is sandboxed, so it can only reach the app's snapshot through an App
# Group. macOS honours `com.apple.security.application-groups` on an ad-hoc
# signature — no provisioning profile, no developer account. But Xcode's *build
# system* refuses to sign a target carrying any entitlement unless it can resolve
# a profile ("requires a provisioning profile"). So when there's no team we ask
# xcodebuild for an unsigned bundle and sign it ourselves, inside out.
set -euo pipefail
cd "$(dirname "$0")"

CONFIG="${CONFIG:-Release}"
DERIVED="$PWD/.build"
APP="$DERIVED/Build/Products/$CONFIG/Runway.app"
INSTALL=0
[[ "${1:-}" == "--install" ]] && INSTALL=1

command -v xcodegen >/dev/null || { echo "xcodegen not found — brew install xcodegen"; exit 1; }

echo "==> Generating Runway.xcodeproj"
xcodegen generate --quiet --spec project.yml

BUILD_ARGS=(ONLY_ACTIVE_ARCH=NO)
MANUAL_SIGN=""
if [[ -n "${DEVELOPMENT_TEAM:-}" ]]; then
  echo "==> Signing: Xcode-managed, team $DEVELOPMENT_TEAM"
  BUILD_ARGS+=(CODE_SIGN_STYLE=Automatic "DEVELOPMENT_TEAM=$DEVELOPMENT_TEAM" -allowProvisioningUpdates)
else
  MANUAL_SIGN="${SIGN_IDENTITY:--}"
  if [[ "$MANUAL_SIGN" == "-" ]]; then
    echo "==> Signing: ad-hoc, applied after the build (no Apple account required)"
  else
    echo "==> Signing: '$MANUAL_SIGN', applied after the build"
  fi
  BUILD_ARGS+=(CODE_SIGNING_ALLOWED=NO CODE_SIGNING_REQUIRED=NO ENTITLEMENTS_REQUIRED=NO
               CODE_SIGN_IDENTITY="" DEVELOPMENT_TEAM="")
fi

echo "==> Building ($CONFIG)"
xcodebuild \
  -project Runway.xcodeproj \
  -scheme Runway \
  -configuration "$CONFIG" \
  -derivedDataPath "$DERIVED" \
  "${BUILD_ARGS[@]}" \
  build | grep -E "error:|warning:|BUILD" || true

[[ -d "$APP" ]] || { echo "Build failed — no app produced."; exit 1; }

if [[ -n "$MANUAL_SIGN" ]]; then
  echo "==> Codesigning (nested code first, then the app that contains it)"
  codesign -f -s "$MANUAL_SIGN" --timestamp=none \
    --entitlements Sources/Widget/RunwayWidget.entitlements \
    "$APP/Contents/PlugIns/RunwayWidget.appex"
  codesign -f -s "$MANUAL_SIGN" --timestamp=none \
    --entitlements Sources/App/Runway.entitlements \
    "$APP"
  codesign --verify --deep --strict "$APP"
fi

echo
echo "Built: $APP"

if [[ $INSTALL -eq 1 ]]; then
  # The widget only becomes visible in the gallery once the containing app has
  # been launched from a location the system scans. /Applications is the reliable
  # one; launching straight out of .build does not register the extension.
  echo "==> Installing to /Applications"
  osascript -e 'quit app "Runway"' 2>/dev/null || true
  sleep 1
  rm -rf /Applications/Runway.app
  cp -R "$APP" /Applications/
  open /Applications/Runway.app
  echo
  echo "Installed and launched. The widget appears in the gallery a few seconds"
  echo "after first launch: right-click the desktop → Edit Widgets → Runway."
else
  echo "Install: cp -R '$APP' /Applications/ && open /Applications/Runway.app"
  echo "         (the widget is only registered once the app runs from /Applications)"
fi
