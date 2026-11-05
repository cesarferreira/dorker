#!/usr/bin/env bash
# Dev loop for the counter example: Gradle -> dorker build -> run -> instrumentation tests -> cleanup.
#   ./dev.sh            full loop
#   DORKER=../../target/debug/dorker ./dev.sh   use a locally built dorker
set -euo pipefail
cd "$(dirname "$0")"
DORKER="${DORKER:-dorker}"
RUNNER="com.dorker.counter.test/androidx.test.runner.AndroidJUnitRunner"

./gradlew -q assembleDebug assembleDebugAndroidTest
"$DORKER" build -t counter:dev .
"$DORKER" run -d --name counter-dev counter:dev
trap '"$DORKER" rm -f counter-dev >/dev/null' EXIT
"$DORKER" screenshot counter-dev counter-dev.png
"$DORKER" test counter-dev --runner "$RUNNER"
