#!/bin/bash
# Regenerate desktop shipping assets from AppIcon.icon. Run on macOS with Xcode.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
DEVELOPER="$(xcode-select -p)"
IC="${DEVELOPER_DIR:-$DEVELOPER}/../Applications/Icon Composer.app/Contents/Executables/ictool"
OUT="$(mktemp -d "${TMPDIR:-/tmp}/approved-icons.XXXXXX")"
for bundle in AppIcon.icon AppIcon-cobalt.icon; do
  cp "$HERE/Sources/Front.svg" "$HERE/$bundle/Assets/Front.svg"
  cp "$HERE/Sources/Middle.svg" "$HERE/$bundle/Assets/Middle.svg"
done
"$IC" "$HERE/AppIcon.icon" --export-image --output-file "$OUT/default.png" \
  --platform macOS --rendition Default --width 1024 --height 1024 --scale 1 --design-generation 27
"$ROOT/node_modules/.bin/tauri" icon "$OUT/default.png" --output "$OUT/fallback"
# Retain platform-native mobile sources; these exports use the macOS mask.
for asset in "$OUT/fallback"/*; do
  [ -f "$asset" ] || continue
  cp "$asset" "$ROOT/src-tauri/icons/"
done
xcrun actool "$HERE/AppIcon.icon" --compile "$OUT" --platform macosx \
  --minimum-deployment-target 13.0 --target-device mac --app-icon AppIcon \
  --output-partial-info-plist "$OUT/icon-info.plist" --warnings --errors
cp "$OUT/Assets.car" "$ROOT/src-tauri/Assets.car"
cp "$OUT/fallback/32x32.png" "$ROOT/static/favicon.png"
if [ -d "$ROOT/native" ]; then
  cp "$OUT/fallback/icon.icns" "$ROOT/native/AppIcon.icns"
  cp "$OUT/fallback/icon.icns" "$ROOT/native/Sources/BrewBrowserKit/Resources/AppIcon.icns"
fi
# In-app About-modal asset, both appearances. AboutModal swaps on the app's
# resolved [data-theme], so the light plate no longer sits in a dark window.
#
# The raw ictool exports are 1024x1024 16-bit (~1.3MB each) and the modal renders
# them at 80px, so downscale to 256: past any real display density, 8-bit, ~40KB.
# Without this the two icons alone would outweigh the rest of the frontend bundle.
if [ -f "$ROOT/src/lib/assets/app-icon.png" ]; then
  "$IC" "$HERE/AppIcon.icon" --export-image --output-file "$OUT/dark.png" \
    --platform macOS --rendition Dark --width 1024 --height 1024 --scale 1 --design-generation 27
  cp "$OUT/default.png" "$ROOT/src/lib/assets/app-icon.png"
  cp "$OUT/dark.png"    "$ROOT/src/lib/assets/app-icon-dark.png"
  sips -Z 256 "$ROOT/src/lib/assets/app-icon.png" "$ROOT/src/lib/assets/app-icon-dark.png" >/dev/null
fi
echo "Desktop icons regenerated. Inspection artifacts: $OUT"
