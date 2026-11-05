#!/usr/bin/env bash
# Builds app.apk (target app with one launcher activity) and test.apk (instrumentation) with the Android SDK build-tools and a JDK.
set -euo pipefail
cd "$(dirname "$0")"
SDK="${ANDROID_HOME:-$HOME/Library/Android/sdk}"
BT="$(ls -d "$SDK"/build-tools/* | sort -V | tail -1)"
JAR="$(ls -d "$SDK"/platforms/android-* | sort -V | tail -1)/android.jar"
KS="${TMPDIR:-/tmp}/dorker-sample.keystore"
[ -f "$KS" ] || keytool -genkeypair -keystore "$KS" -storepass android -keypass android \
  -alias dorker -keyalg RSA -keysize 2048 -validity 10000 -dname "CN=dorker sample" >/dev/null 2>&1
W="$(mktemp -d)"; trap 'rm -rf "$W"' EXIT
sign() { "$BT/zipalign" -f 4 "$1" "$W/aligned.apk"
  "$BT/apksigner" sign --ks "$KS" --ks-pass pass:android --key-pass pass:android --ks-key-alias dorker --out "$2" "$W/aligned.apk"
  rm -f "$2.idsig"; }

mkdir -p "$W/app-classes"
javac --release 8 -Xlint:-options -cp "$JAR" -d "$W/app-classes" src/com/dorker/sample/MainActivity.java
mkdir -p "$W/app-dex"
"$BT/d8" --lib "$JAR" --output "$W/app-dex" $(find "$W/app-classes" -name '*.class')
"$BT/aapt2" link --manifest AppManifest.xml -I "$JAR" -o "$W/app.apk"
(cd "$W/app-dex" && zip -q -j "$W/app.apk" classes.dex)
sign "$W/app.apk" app.apk

mkdir -p "$W/classes"
javac --release 8 -Xlint:-options -cp "$JAR" -d "$W/classes" src/com/dorker/sample/SampleRunner.java
"$BT/d8" --lib "$JAR" --output "$W" $(find "$W/classes" -name '*.class')
"$BT/aapt2" link --manifest TestManifest.xml -I "$JAR" -o "$W/test.apk"
(cd "$W" && zip -q -j test.apk classes.dex)
sign "$W/test.apk" test.apk
echo "built $(pwd)/app.apk and test.apk"
