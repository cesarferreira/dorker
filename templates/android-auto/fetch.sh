#!/usr/bin/env bash
# Downloads Android Auto (com.google.android.projection.gearhead) as an .xapk with apkeep
# (https://github.com/EFForg/apkeep, `cargo install apkeep`). The file is not committed:
# it is Google's app, about 50 MB, and the version changes.
#
# `dorker run` / `dorker build` run this for you (the FETCH line in the Dorkerfile) when the
# file is missing. Run it by hand, or delete the file, to get a newer version.
set -euo pipefail
cd "$(dirname "$0")"
command -v apkeep >/dev/null || { echo "apkeep not found: cargo install apkeep" >&2; exit 1; }
apkeep -a com.google.android.projection.gearhead -d apk-pure .
ls -l com.google.android.projection.gearhead.xapk
