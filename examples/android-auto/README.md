# Android Auto example

Installs Google's Android Auto app (`com.google.android.projection.gearhead`) from an
`.xapk` bundle, which is how it is usually distributed: a base APK plus split APKs for the
CPU architecture, language and screen density.

```bash
cargo install apkeep        # once
./fetch.sh                  # downloads the Android Auto .xapk

dorker build -t gearhead .  # installs the bundle, all splits together
dorker run -d --name auto gearhead
dorker exec auto "pm list packages | grep gearhead"
dorker rm -f auto
```

Replace the `.xapk` with a newer one and the next `dorker build` or `dorker run` makes a
new image, because the file's hash is part of the image identity.

## Setting up the head unit server (once)

Android Auto's developer mode is a setting inside the app, so installing the APK does not
turn it on, and the head unit server (what a head unit emulator connects to) does not run
until it is started. `enable-dev-mode.sh` does both on a running instance, then you save
the instance as an image:

```bash
dorker run -d --name setup                # builds this Dorkerfile and runs it
./enable-dev-mode.sh setup                # developer mode on, server started
dorker commit setup -t gearhead:ready
dorker run -d --name auto gearhead:ready  # the server is already up
```

The script drives the app's UI over adb. It finds each button by its text, not by screen
position, and it is safe to run again. It needs `adb` and `python3`, and the app's UI must
be in English. A new instance from the committed image has the server already listening
(port 5277 in the guest).

To do it by hand instead: Android Auto has no launcher icon, so open its settings with

```bash
PKG=com.google.android.projection.gearhead
adb -s emulator-5554 shell am start \
  -n $PKG/.companion.settings.DefaultSettingsActivity
```

then scroll to the bottom and tap **Version** about 10 times, choose **OK** in the "Allow
development settings?" dialog, open the **&#8942;** menu and choose **Start head unit server**.

Checked on an Apple Silicon Mac with `android:36-playstore` and Android Auto 17.9: the
server survived the commit and a restore.

Notes:

- It uses `FROM android:36-playstore` on purpose. The plain `google_apis` image carries an
  Android Auto *stub* signed with the AOSP test key, and the real app (signed by Google)
  cannot replace it: `INSTALL_FAILED_UPDATE_INCOMPATIBLE`. The Play Store image's stub is
  signed by Google, so the update installs.
- The bundle from apkeep is for **arm64** (it has `config.arm64_v8a.apk`), so it installs on
  Apple Silicon. On an x86_64 Linux host you need an x86_64 bundle instead.
- The `google_apis` image already has Google Play services, which Android Auto needs.
  Installing the app does not connect a car: use Android Auto's developer mode and a head
  unit (or the Desktop Head Unit) for that.
- The `.xapk` is not committed (Google's app, version changes). Check that your use of it
  complies with its terms.
