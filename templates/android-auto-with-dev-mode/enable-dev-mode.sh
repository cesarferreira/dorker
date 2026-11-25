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
  SERIAL="$(dorker ps --json | python3 -I -c '
import json, sys
for i in json.load(sys.stdin):
    if i["name"] == sys.argv[1] and i["status"] == "READY":
        print(i["serial"])
' "$TARGET")"
  [ -n "$SERIAL" ] || {
    echo "no running dorker instance named '$TARGET' (see: dorker ps)" >&2
    exit 1
  }
fi
adb_() { "$ADB" -s "$SERIAL" "$@"; }
say() { echo "• $*"; }

# Dump the screen's UI tree as XML; print nothing (and succeed) when the dump fails, which
# happens while the screen is still changing.
dump() {
  adb_ shell rm -f /sdcard/dorker-ui.xml >/dev/null 2>&1 || true
  adb_ shell uiautomator dump /sdcard/dorker-ui.xml >/dev/null 2>&1 || true
  adb_ shell cat /sdcard/dorker-ui.xml 2>/dev/null || true
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

# A slow emulator (software rendering, a fresh boot) can show "<app> isn't responding" over
# everything, and the taps then hit the dialog instead of the app. A hung System UI keeps
# coming back while it waits, so close it (Android restarts it at once); for any other app,
# wait.
dismiss_anr() {
  local screen pos button
  for _ in 1 2 3 4 5; do
    screen="$(dump)"
    grep -q "isn't responding\|isn&apos;t responding" <<<"$screen" || return 0
    if grep -q "System UI isn" <<<"$screen"; then button="Close app"; else button="Wait"; fi
    pos="$(center_of "$button" <<<"$screen")"
    [ -n "$pos" ] || return 0
    say "an app is not responding: tapping \"$button\""
    # shellcheck disable=SC2086
    tap $pos
    sleep 3
  done
}

# Wait until the device has gone a few seconds without a "not responding" dialog: right
# after a boot, software rendering keeps System UI busy for a while.
settle() {
  local calm=0
  for _ in $(seq 1 40); do
    if dump | grep -q "isn't responding\|isn&apos;t responding"; then
      calm=0
      dismiss_anr
    else
      calm=$((calm + 1))
      [ "$calm" -ge 3 ] && return 0
    fi
    sleep 2
  done
}

# Tap the element labelled $1 if it is on screen; fail quietly (return 1) if it is not.
tap_label() {
  local pos
  pos="$(dump | center_of "$1")"
  [ -n "$pos" ] || return 1
  # shellcheck disable=SC2086
  tap $pos
}

open_settings() {
  dismiss_anr
  adb_ shell am force-stop "$PKG" >/dev/null
  adb_ shell am start -n "$SETTINGS" >/dev/null
  # Wait for the list to draw (the toolbar alone says "Android Auto" too early).
  for _ in $(seq 1 30); do
    dismiss_anr
    dump | grep -q 'Connect a vehicle\|Messaging' && return 0
    sleep 0.5
  done
  echo "Android Auto's settings did not open (is the app installed?)" >&2
  return 1
}

# Open the overflow menu and tap "Start head unit server" if it is offered. The menu can
# ignore a tap while the screen is still settling, so try a few times; a menu without the
# entry (developer mode off) is closed again.
start_server() {
  for _ in 1 2 3 4; do
    dismiss_anr
    tap_label "More options" || { sleep 1; continue; }
    sleep 1
    if tap_label "Start head unit server"; then return 0; fi
    if dump | grep -q 'Help &amp; feedback\|Help & feedback'; then
      adb_ shell input keyevent KEYCODE_BACK
      return 1
    fi
  done
  return 1
}

say "waiting for $SERIAL to settle"
settle
say "opening Android Auto settings on $SERIAL"
open_settings

if ! start_server; then
  say "developer mode is off: enabling it"
  # "Version" ("Version and permission info" in 18.x) is at the bottom of the list.
  found=
  for _ in $(seq 1 10); do
    if [ -n "$(dump | center_of "Version")" ]; then found=1; break; fi
    adb_ shell input swipe 540 2000 540 600 250
    sleep 0.4
  done
  [ -n "$found" ] || { echo "could not find the Version row" >&2; exit 1; }
  # In 18.x the first tap expands the package details under the row, which moves the row.
  # So tap once, scroll to the very end, let the list settle (taps during a fling only stop
  # it), and find the row again before the real taps.
  pos="$(dump | center_of "Version")"
  # shellcheck disable=SC2086
  tap $pos
  sleep 1
  for _ in 1 2 3; do adb_ shell input swipe 540 2000 540 600 200; done
  sleep 1.5
  pos="$(dump | center_of "Version")"
  [ -n "$pos" ] || { echo "lost the Version row" >&2; exit 1; }
  # Ten quick taps in one adb call: separate calls are too slow on a slow emulator, and
  # the app then counts them as unrelated taps.
  set -- $pos
  adb_ shell "for i in 1 2 3 4 5 6 7 8 9 10; do input tap $1 $2; done"
  sleep 1.5
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
