#!/usr/bin/env bash
# The whole round trip on one machine, without an account anywhere: a throwaway registry in
# Docker stands in for ghcr.io, and two separate dorker stores stand in for you and a
# teammate. Needs Docker, the Android SDK and about 10 GB free; takes a few minutes.
#
#   ./try-locally.sh
#   DORKER=../../target/release/dorker ./try-locally.sh   # a locally built dorker
set -euo pipefail
cd "$(dirname "$0")"
DORKER="${DORKER:-dorker}"
PORT="${PORT:-5055}"
REF="localhost:$PORT/team/team-env:1"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/dorker-shared-image.XXXXXX")"

cleanup() {
  DORKER_HOME="$WORK/teammate" "$DORKER" rm -f team-dev >/dev/null 2>&1 || true
  docker rm -f dorker-example-registry >/dev/null 2>&1 || true
  rm -rf "$WORK"
}
trap cleanup EXIT

echo "• starting a throwaway registry on localhost:$PORT"
docker run -d --rm -p "$PORT:5000" --name dorker-example-registry registry:2 >/dev/null

echo "• you: build and push $REF"
DORKER_HOME="$WORK/you" DORKER="$DORKER" ./publish.sh "$REF"

echo "• a teammate with an empty store: run it"
DORKER_HOME="$WORK/teammate" DORKER="$DORKER" ./dev.sh "$REF"
DORKER_HOME="$WORK/teammate" "$DORKER" images
