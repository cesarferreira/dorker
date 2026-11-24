#!/usr/bin/env bash
# Build this folder's Dorkerfile and push it to a registry.
#
#   ./publish.sh ghcr.io/your-org/team-env:1
#
# Run it on an Apple Silicon Mac and on an x86_64 Linux machine (or CI): each push adds its
# own CPU architecture to the tag, and `dorker pull` picks the one each machine can run.
# Log in first; for ghcr.io: gh auth refresh -s write:packages
set -euo pipefail
cd "$(dirname "$0")"
DORKER="${DORKER:-dorker}"
REF="${1:?usage: ./publish.sh <registry>/<repository>:<tag>, e.g. ghcr.io/your-org/team-env:1}"

"$DORKER" build -t team-env:local .
"$DORKER" push team-env:local "$REF"
