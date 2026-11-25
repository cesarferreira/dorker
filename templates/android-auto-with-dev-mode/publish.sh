#!/usr/bin/env bash
# Publishes this folder's image with the latest Android Auto to a registry, tagged
# <repository>:<api>-aa<major.minor> (for example ghcr.io/your-org/gearhead:37-aa18.1). The
# <repository>:<api> tag moves to it when it is the newest Android Auto version there.
#
# By default it updates the previous version, like a device that gets Android Auto from the
# Play Store: it runs the newest published <api>-aa<older> image, installs the new Android
# Auto over it (app data and developer mode are kept), starts the head unit server and saves
# the result. With --fresh, or when there is no previous version, it builds the Dorkerfile
# from scratch (a clean install).
#
# Run the same command on an Apple Silicon Mac and on an x86_64 Linux box (at the same
# time is fine). Each one adds its own CPU architecture to the tags, and `dorker run` picks
# the one for the machine it runs on:
#
#   ./publish.sh ghcr.io/your-org/gearhead            # uses the Dorkerfile's FROM
#   ./publish.sh ghcr.io/your-org/gearhead --api 37   # sets FROM android:37-playstore first
#   ./publish.sh ghcr.io/your-org/gearhead --fresh    # clean install instead of an update
#
# A version whose tag already has this machine's architecture is not published again (pass
# --force to redo it). Needs dorker, apkeep (`cargo install apkeep`), python3 and unzip;
# for ghcr.io also the GitHub CLI, logged in with `gh auth refresh -s write:packages`.
set -euo pipefail
cd "$(dirname "$0")"

usage() { echo "usage: ./publish.sh <registry>/<repository> [--api N] [--fresh] [--force]" >&2; exit 2; }
die() { echo "error: $*" >&2; exit 1; }
step() { printf '\n\033[1m==> %s\033[0m\n' "$*"; }

REPO_REF="" API="" FORCE="" FRESH=""
while [ $# -gt 0 ]; do
  case "$1" in
    --api) API="${2:?--api needs a number}"; shift 2 ;;
    --force) FORCE=1; shift ;;
    --fresh) FRESH=1; shift ;;
    -h|--help) usage ;;
    -*) usage ;;
    *) [ -z "$REPO_REF" ] || usage; REPO_REF="$1"; shift ;;
  esac
done
[ -n "$REPO_REF" ] || usage
case "$REPO_REF" in
  */*:*/* | */*) ;;
  *) die "'$REPO_REF' is not <registry>/<repository>, e.g. ghcr.io/your-org/gearhead" ;;
esac
case "${REPO_REF##*/}" in *:*) die "give the repository without a tag: the script picks the tags" ;; esac
REGISTRY="${REPO_REF%%/*}"
REPOSITORY="${REPO_REF#*/}"

# This machine's architecture, as the registry names it.
case "$(uname -s)/$(uname -m)" in
  Darwin/arm64) ARCH=arm64 ;;
  Linux/x86_64) ARCH=amd64 ;;
  *) die "run this on an Apple Silicon Mac or an x86_64 Linux machine (this is $(uname -s) $(uname -m))" ;;
esac

for c in dorker python3 unzip; do command -v "$c" >/dev/null || die "$c not found"; done
command -v apkeep >/dev/null || die "apkeep not found: cargo install apkeep"
dorker push --help >/dev/null 2>&1 || die "this dorker has no push command: update it"

# Credentials for the registry checks below (dorker push finds its own).
REG_USER="${DORKER_REGISTRY_USER:-}" REG_TOKEN="${DORKER_REGISTRY_TOKEN:-}"
if [ -z "$REG_TOKEN" ] && [ "$REGISTRY" = ghcr.io ]; then
  command -v gh >/dev/null || die "gh not found (needed for ghcr.io): https://cli.github.com"
  gh auth status 2>&1 | grep -q 'write:packages' ||
    die "your gh login cannot push packages: run gh auth refresh -s write:packages"
  REG_TOKEN="${GITHUB_TOKEN:-${GH_TOKEN:-$(gh auth token)}}"
  REG_USER="$(gh api user --jq .login)"
fi
export REG_USER REG_TOKEN

# registry arches <tag>  -> "<architecture> <manifest digest>" per line (nothing if no tag)
# registry tags          -> every tag of the repository, one per line
registry() {
  python3 -I -c '
import base64, json, os, re, sys, urllib.error, urllib.parse, urllib.request
reg, repo, cmd = sys.argv[1:4]
arg = sys.argv[4] if len(sys.argv) > 4 else ""
host = reg.split(":")[0]
scheme = "http" if host in ("localhost", "127.0.0.1") else "https"
user, token = os.environ.get("REG_USER", ""), os.environ.get("REG_TOKEN", "")
accept = ", ".join([
    "application/vnd.oci.image.index.v1+json",
    "application/vnd.oci.image.manifest.v1+json",
])
basic = "Basic " + base64.b64encode(f"{user}:{token}".encode()).decode() if token else None

def fetch(url, auth):
    headers = {"Accept": accept}
    if auth:
        headers["Authorization"] = auth
    with urllib.request.urlopen(urllib.request.Request(url, headers=headers), timeout=60) as r:
        return json.load(r)

def get(path):
    url = f"{scheme}://{reg}/v2/{repo}/{path}"
    try:
        return fetch(url, None)
    except urllib.error.HTTPError as e:
        if e.code == 404:
            return None
        if e.code != 401:
            raise
        challenge = e.headers.get("WWW-Authenticate", "")
    if challenge.lower().startswith("basic"):
        auth = basic
    else:
        p = dict(re.findall(r"(\w+)=\"([^\"]*)\"", challenge))
        q = {"service": p.get("service", host), "scope": f"repository:{repo}:pull"}
        req = urllib.request.Request(p["realm"] + "?" + urllib.parse.urlencode(q))
        if basic:
            req.add_header("Authorization", basic)
        with urllib.request.urlopen(req, timeout=60) as r:
            t = json.load(r)
        auth = "Bearer " + (t.get("token") or t.get("access_token"))
    try:
        return fetch(url, auth)
    except urllib.error.HTTPError as e:
        if e.code == 404:
            return None
        raise

if cmd == "arches":
    m = get(f"manifests/{arg}") or {}
    for d in m.get("manifests", []):
        a = (d.get("platform") or {}).get("architecture")
        if a:
            print(a, d.get("digest", ""))
elif cmd == "tags":
    for t in (get("tags/list?n=10000") or {}).get("tags") or []:
        print(t)
' "$REGISTRY" "$REPOSITORY" "$@"
}

# The manifest digest of this machine's architecture under tag $1, or nothing.
digest_of() { registry arches "$1" | awk -v a="$ARCH" '$1 == a { print $2 }'; }

# version_tags <api> [<major.minor>]: the registry's "<api>-aa<major.minor>" tags, newest
# first; with a version, only those older than it.
version_tags() {
  registry tags | python3 -I -c '
import re, sys
api, below = sys.argv[1], sys.argv[2] if len(sys.argv) > 2 else ""
limit = tuple(map(int, below.split("."))) if below else None
found = []
for t in sys.stdin.read().split():
    m = re.fullmatch(re.escape(api) + r"-aa(\d+)\.(\d+)", t)
    if m and (limit is None or (int(m[1]), int(m[2])) < limit):
        found.append(((int(m[1]), int(m[2])), t))
for _, t in sorted(found, reverse=True):
    print(t)
' "$@"
}

step "Dorkerfile"
if [ -n "$API" ]; then
  sed -i.bak "s/^FROM .*/FROM android:$API-playstore/" Dorkerfile && rm -f Dorkerfile.bak
fi
FROM="$(sed -n 's/^FROM[[:space:]]*//p' Dorkerfile | head -1)"
API="$(echo "$FROM" | sed -n 's/^android:\([0-9][0-9]*\).*/\1/p')"
[ -n "$API" ] || die "could not read the API level from 'FROM $FROM'"
echo "FROM $FROM, $ARCH"

step "Downloading the latest Android Auto"
XAPK=com.google.android.projection.gearhead.xapk
rm -f "$XAPK"
./fetch.sh
AA_VERSION="$(unzip -p "$XAPK" manifest.json |
  python3 -I -c 'import json, sys; print(json.load(sys.stdin)["version_name"])')"
AA_SHORT="$(echo "$AA_VERSION" | cut -d. -f1,2)"
TAG="$API-aa$AA_SHORT"
IMAGE="$REGISTRY/$REPOSITORY:$TAG"
echo "Android Auto $AA_VERSION -> $IMAGE"

# The image to update: the newest older version that has this machine's architecture.
BASE="" UPDATED_FROM=""
if [ -z "$FRESH" ]; then
  for t in $(version_tags "$API" "$AA_SHORT"); do
    if [ -n "$(digest_of "$t")" ]; then BASE="$REGISTRY/$REPOSITORY:$t"; break; fi
  done
fi

if [ -z "$FORCE" ] && [ -n "$(digest_of "$TAG")" ]; then
  echo "$IMAGE already has $ARCH: nothing to publish (--force redoes it)"
else
  LOCAL="aa-publish:$TAG"
  if [ -n "$BASE" ]; then
    step "Updating $BASE to Android Auto $AA_VERSION"
    WORK="aa-publish-update-$$"
    trap 'dorker rm -f "$WORK" >/dev/null 2>&1 || true' EXIT
    dorker run -d --name "$WORK" "$BASE"   # pulls it the first time
    dorker install "$WORK" "$XAPK"         # an in-place update: app data is kept
    ./enable-dev-mode.sh "$WORK"           # restarts the head unit server (and dev mode if lost)
    dorker commit "$WORK" -t "$LOCAL"
    UPDATED_FROM="$BASE"
    dorker rm -f "$WORK" >/dev/null
    trap - EXIT
  else
    [ -n "$FRESH" ] || echo "no older $API-aa* image for $ARCH: building from scratch"
    step "Installing the system image (skipped when installed)"
    dorker pull "$FROM"

    step "Building (installs Android Auto, turns on developer mode, saves the snapshot)"
    dorker build -t "$LOCAL" .
  fi

  step "Checking the image"
  CHECK="aa-publish-check-$$"
  trap 'dorker rm -f "$CHECK" >/dev/null 2>&1 || true' EXIT
  dorker run -d --name "$CHECK" "$LOCAL" >/dev/null
  dorker exec "$CHECK" "netstat -tln | grep -q ':5277 '" ||
    die "the head unit server is not listening on port 5277: not pushing"
  GOT="$(dorker exec "$CHECK" "dumpsys package com.google.android.projection.gearhead" |
    sed -n 's/.*versionName=\([^ ]*\).*/\1/p' | head -1)"
  [ "$GOT" = "$AA_VERSION" ] || die "Android Auto in the image is '$GOT', expected $AA_VERSION: not pushing"
  dorker rm -f "$CHECK" >/dev/null
  trap - EXIT
  echo "Android Auto $GOT, head unit server listening on 5277"

  step "Pushing $IMAGE ($ARCH)"
  dorker push "$LOCAL" "$IMAGE"
  dorker rmi "$LOCAL" >/dev/null   # only drops the local name; the image keeps its pushed tag
fi

# Move <api> to this version only when no newer Android Auto version is published. Two
# machines running at once with different versions then cannot move it back.
NEWEST="$(version_tags "$API" | head -1 || true)"
MOVING="$REGISTRY/$REPOSITORY:$API"
if [ "$NEWEST" = "$TAG" ]; then
  if [ "$(digest_of "$API")" = "$(digest_of "$TAG")" ]; then
    echo "$MOVING already points to $TAG for $ARCH"
  else
    step "Moving $MOVING to $TAG ($ARCH)"
    dorker images --json | grep -q "\"$IMAGE\"" || dorker pull "$IMAGE"
    dorker push "$IMAGE" "$MOVING"
  fi
elif [ -n "$NEWEST" ]; then
  echo "not moving $MOVING: $NEWEST is newer than $TAG"
else
  echo "could not list the registry's tags: $MOVING not moved"
fi

step "Published${UPDATED_FROM:+ (updated from $UPDATED_FROM)}"
for t in "$TAG" "$API"; do
  have="$(registry arches "$t" | awk '{ print $1 }' | sort | tr '\n' ' ')"
  echo "  $REGISTRY/$REPOSITORY:$t  ${have:-(missing)}"
done
[ "$(registry arches "$TAG" | awk '{ print $1 }' | sort | tr '\n' ' ')" = "amd64 arm64 " ] ||
  echo "  $TAG does not have both amd64 and arm64 yet: run this on the other machine too"
echo
echo "Run it with: dorker run -d --name aa $IMAGE"
