#!/bin/bash
# Build Runway.app.
#
#   ./build.sh                      ad-hoc signed, menu bar app only
#   DEVELOPMENT_TEAM=ABCDE12345 ./build.sh    real team, widget works too
#
# The widget's App Group entitlement needs a real signing team. Without one the
# menu bar app still builds and runs; only the Notification Centre widget is
# unavailable.
set -euo pipefail
cd "$(dirname "$0")"

CONFIG="${CONFIG:-Release}"
DERIVED="$PWD/.build"

command -v xcodegen >/dev/null || { echo "xcodegen not found — brew install xcodegen"; exit 1; }

SIGN_ARGS=("ONLY_ACTIVE_ARCH=NO")
if [[ -n "${DEVELOPMENT_TEAM:-}" ]]; then
  echo "==> Team $DEVELOPMENT_TEAM — building app + widget"
  SPEC=project-widget.yml
  SIGN_ARGS+=(CODE_SIGN_STYLE=Automatic "DEVELOPMENT_TEAM=$DEVELOPMENT_TEAM" -allowProvisioningUpdates)
else
  echo "==> No DEVELOPMENT_TEAM — ad-hoc signing, menu bar app only"
  echo "    (the widget needs a signing team; everything else works without one)"
  SPEC=project.yml
fi

echo "==> Generating Runway.xcodeproj from $SPEC"
xcodegen generate --quiet --spec "$SPEC"

echo "==> Building ($CONFIG)"
xcodebuild \
  -project Runway.xcodeproj \
  -scheme Runway \
  -configuration "$CONFIG" \
  -derivedDataPath "$DERIVED" \
  "${SIGN_ARGS[@]}" \
  build | grep -E "error:|warning:|BUILD|Compiling|Signing" || true

APP="$DERIVED/Build/Products/$CONFIG/Runway.app"
if [[ ! -d "$APP" ]]; then
  echo "Build failed — no app produced."
  exit 1
fi

echo
echo "Built: $APP"
echo "Run:   open '$APP'"
echo "Install: cp -R '$APP' /Applications/"
