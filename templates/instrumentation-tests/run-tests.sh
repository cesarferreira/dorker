#!/usr/bin/env bash
# Builds the app and its instrumentation tests with Gradle, runs the tests on a fresh dorker
# instance, and removes the instance afterwards.
#
#   ./run-tests.sh com.example.app.test/androidx.test.runner.AndroidJUnitRunner
#
# The runner is `<test package>/<instrumentation runner>`; RUNNER=... works too.
# SKIP_BUILD=1 skips Gradle (use APKs you built already).
# The test logs are copied to ./test-results (RESULTS=dir to change it), so they survive
# the instance being removed; upload that folder as a CI artifact.
# Exit status: 0 all passed, 7 a test failed (dorker's code), anything else is an error.
set -euo pipefail
cd "$(dirname "$0")"

RUNNER="${1:-${RUNNER:-}}"
if [ -z "$RUNNER" ]; then
  echo "usage: ./run-tests.sh <test-package>/<instrumentation-runner>" >&2
  exit 2
fi

if [ -z "${SKIP_BUILD:-}" ]; then
  ./gradlew assembleDebug assembleDebugAndroidTest
fi

NAME="tests-$$"
trap 'dorker rm -f "$NAME" >/dev/null 2>&1 || true' EXIT

# Builds the Dorkerfile in this folder (cached while the APKs do not change), then starts it.
dorker run -d --name "$NAME"
dorker test "$NAME" --runner "$RUNNER" --out "${RESULTS:-test-results}"
