#!/usr/bin/env bash
# Turns on Android Auto's developer mode and starts its head unit server on a running
# dorker instance, by driving the app's UI over adb. Run it once, then save the result:
#
#   dorker run -d --name setup
#   ./enable-dev-mode.sh setup
#   dorker commit setup -t gearhead:ready
#
# Usage: ./enable-dev-mode.sh [instance-name-or-adb-serial]   (default: setup)
#
# Also usable as a Dorkerfile step, which runs it once when the image is built:
#   PROVISION ./enable-dev-mode.sh      (dorker passes the build emulator's serial)
#
# It finds each button by its text (uiautomator), not by screen position, so it does not
# depend on the screen size. The app's UI must be in English. Needs python3 and adb.
set -euo pipefail

TARGET="${1:-setup}"
PKG=com.google.android.projection.gearhead
SETTINGS="$PKG/.companion.settings.DefaultSettingsActivity"
ADB="${ADB:-adb}"
command -v "$ADB" >/dev/null || ADB="$HOME/Library/Android/sdk/platform-tools/adb"
command -v python3 >/dev/null || { echo "python3 not found" >&2; exit 1; }

# An adb serial (emulator-5554) is used as is; anything else is a dorker instance name.
if [[ "$TARGET" == emulator-* ]]; then
  SERIAL="$TARGET"
else
  SERIAL="$(dorker ps | awk -v n="$TARGET" '$2 == n { print $5 }')"
  [ -n "$SERIAL" ] && [ "$SERIAL" != "-" ] || {
    echo "no running dorker instance named '$TARGET' (see: dorker ps)" >&2
    exit 1
  }
fi
adb_() { "$ADB" -s "$SERIAL" "$@"; }
say() { echo "• $*"; }

# Dump the screen's UI tree as XML.
dump() {
  adb_ shell uiautomator dump /sdcard/dorker-ui.xml >/dev/null 2>&1
  adb_ shell cat /sdcard/dorker-ui.xml
}

# Print "x y" for the centre of the first element whose text or content description
# contains $1 (case-insensitive); print nothing if there is none.
center_of() {
  python3 -c '
import re, sys, xml.etree.ElementTree as ET
want = sys.argv[1].lower()
text = sys.stdin.read()
# uiautomator never writes a DTD. Refuse one rather than hand it to the XML parser
# (entity expansion / external entities).
if "<!DOCTYPE" in text or "<!ENTITY" in text:
    sys.exit(0)
try:
    root = ET.fromstring(text)
except ET.ParseError:
    sys.exit(0)
for n in root.iter("node"):
    label = (n.get("text") or "") + "\n" + (n.get("content-desc") or "")
    if want in label.lower():
        m = re.findall(r"\d+", n.get("bounds") or "")
        if len(m) == 4:
            l, t, r, b = map(int, m)
            print((l + r) // 2, (t + b) // 2)
            sys.exit(0)
' "$1"
}

tap() { adb_ shell input tap "$1" "$2"; }

# Tap the element labelled $1 if it is on screen; fail quietly (return 1) if it is not.
tap_label() {
  local pos
  pos="$(dump | center_of "$1")"
  [ -n "$pos" ] || return 1
  # shellcheck disable=SC2086
  tap $pos
}

open_settings() {
  adb_ shell am force-stop "$PKG" >/dev/null
  adb_ shell am start -n "$SETTINGS" >/dev/null
  # Wait for the screen to draw.
  for _ in $(seq 1 20); do
    dump | grep -q 'Connect a vehicle\|Messaging\|Android Auto' && return 0
    sleep 0.5
  done
  echo "Android Auto's settings did not open (is the app installed?)" >&2
  return 1
}

# Open the overflow menu and tap "Start head unit server" if it is offered.
start_server() {
  tap_label "More options" || return 1
  sleep 0.7
  tap_label "Start head unit server"
}

say "opening Android Auto settings on $SERIAL"
open_settings

if ! start_server; then
  say "developer mode is off: enabling it"
  # "Version" is at the bottom of the list.
  found=
  for _ in $(seq 1 10); do
    if [ -n "$(dump | center_of "Version")" ]; then found=1; break; fi
    adb_ shell input swipe 540 2000 540 600 250
    sleep 0.4
  done
  [ -n "$found" ] || { echo "could not find the Version row" >&2; exit 1; }
  pos="$(dump | center_of "Version")"
  # shellcheck disable=SC2086
  for _ in $(seq 1 10); do tap $pos; sleep 0.25; done
  sleep 1
  # "Allow development settings?" -> OK
  tap_label "OK" || { echo "the developer-settings prompt did not appear" >&2; exit 1; }
  sleep 1
  say "developer mode enabled"
  open_settings
  start_server || { echo "could not find 'Start head unit server' in the menu" >&2; exit 1; }
fi

# The head unit server listens on TCP 5277 in the guest.
for _ in $(seq 1 20); do
  if adb_ shell "netstat -tln 2>/dev/null | grep -q ':5277 '"; then
    say "head unit server is listening (port 5277)"
    # Under `PROVISION` dorker saves the result itself; by hand, suggest the commit.
    [ -n "${DORKER_SERIAL:-}" ] || say "now save it: dorker commit ${TARGET} -t gearhead:ready"
    exit 0
  fi
  sleep 0.5
done
echo "the head unit server did not start listening on port 5277" >&2
exit 1
