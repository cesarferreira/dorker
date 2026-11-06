#!/usr/bin/env bash
# Starts the app on a fresh instance and saves screenshots to a folder (default ./screenshots).
#
#   ./screenshots.sh [out-dir]
#
# Add more screens by driving the app between shots with `dorker exec`, for example:
#   dorker exec "$NAME" "input tap 540 1200"
set -euo pipefail
cd "$(dirname "$0")"

OUT="${1:-screenshots}"
mkdir -p "$OUT"

NAME="shots-$$"
trap 'dorker rm -f "$NAME" >/dev/null 2>&1 || true' EXIT

dorker run -d --name "$NAME"
# `dorker screenshot` waits (up to 10 s) for a frame that is not blank.
dorker screenshot "$NAME" "$OUT/launch.png"

# dorker exec "$NAME" "input tap 540 1200"
# sleep 1
# dorker screenshot "$NAME" "$OUT/second-screen.png"

echo "Saved screenshots to $OUT/"
