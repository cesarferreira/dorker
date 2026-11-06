# Android Auto

Installs Google's Android Auto app (`com.google.android.projection.gearhead`) from an
`.xapk` bundle, which is how it is usually distributed: a base APK plus split APKs for the
CPU architecture, language and screen density. This template stops there: it does **not**
turn on Android Auto's developer mode. If you want that done for you (developer mode on
and the head unit server running), use `android-auto-with-dev-mode` instead.

```bash
cargo install apkeep       # once
./fetch.sh                 # downloads the Android Auto .xapk
dorker run -d --name auto  # builds this Dorkerfile, then runs it
dorker exec auto "pm list packages | grep gearhead"
```

Replace the `.xapk` with a newer one and the next `dorker build` or `dorker run` makes a
new image, because the file's hash is part of the image identity.

Notes:

- It uses `FROM android:36-playstore` on purpose. The plain `google_apis` image carries an
  Android Auto *stub* signed with the AOSP test key, and the real app (signed by Google)
  cannot replace it: `INSTALL_FAILED_UPDATE_INCOMPATIBLE`. The Play Store image's stub is
  signed by Google, so the update installs.
- The bundle from apkeep is for **arm64** (it has `config.arm64_v8a.apk`), so it installs on
  Apple Silicon. On an x86_64 Linux host you need an x86_64 bundle instead.
- Android Auto has no launcher icon. Open its settings with
  `adb shell am start -n com.google.android.projection.gearhead/.companion.settings.DefaultSettingsActivity`
  (find the serial with `dorker ps`).
- The `.xapk` is not committed (Google's app, version changes). Check that your use of it
  complies with its terms.
