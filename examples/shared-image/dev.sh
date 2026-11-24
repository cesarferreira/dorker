#!/usr/bin/env bash
# Start a device from the shared image, install your build on it, and leave it running.
#
#   ./dev.sh ghcr.io/your-org/team-env:1 [path/to/app-debug.apk]
#
# The first run downloads the image (`dorker run` pulls what is not here yet); later runs
# start from the local copy in seconds. `dorker pull <ref>` fetches a newer push of the tag.
# Reinstall after each build with `dorker install team-dev <apk>` (or ./gradlew installDebug
# against the serial `dorker ps` shows). `dorker rm -f team-dev` throws the device away.
set -euo pipefail
DORKER="${DORKER:-dorker}"
REF="${1:?usage: ./dev.sh <registry>/<repository>:<tag> [apk]}"
APK="${2:-}"
NAME="${NAME:-team-dev}"

"$DORKER" run -d --name "$NAME" "$REF"
if [ -n "$APK" ]; then
  "$DORKER" install "$NAME" "$APK"
fi
"$DORKER" exec "$NAME" "settings get global window_animation_scale" \
  | grep -qx 0 && echo "• $NAME is up with the shared settings (animations off)"
"$DORKER" ps
