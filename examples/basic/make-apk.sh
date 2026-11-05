#!/usr/bin/env bash
# Builds a minimal, code-free, debug-signed sample APK (sample.apk) with the Android SDK build-tools.
set -euo pipefail
cd "$(dirname "$0")"
SDK="${ANDROID_HOME:-$HOME/Library/Android/sdk}"
BT="$(ls -d "$SDK"/build-tools/* | sort -V | tail -1)"
JAR="$(ls -d "$SDK"/platforms/android-* | sort -V | tail -1)/android.jar"
KS="${TMPDIR:-/tmp}/dorker-sample.keystore"
[ -f "$KS" ] || keytool -genkeypair -keystore "$KS" -storepass android -keypass android \
  -alias dorker -keyalg RSA -keysize 2048 -validity 10000 -dname "CN=dorker sample" >/dev/null 2>&1
"$BT/aapt2" link --manifest AndroidManifest.xml -I "$JAR" -o sample-unsigned.apk
"$BT/zipalign" -f 4 sample-unsigned.apk sample-aligned.apk
"$BT/apksigner" sign --ks "$KS" --ks-pass pass:android --key-pass pass:android --ks-key-alias dorker --out sample.apk sample-aligned.apk
rm -f sample-unsigned.apk sample-aligned.apk sample.apk.idsig
echo "built $(pwd)/sample.apk"
