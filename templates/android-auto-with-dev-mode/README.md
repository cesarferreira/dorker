# Android Auto with developer mode

Installs Google's Android Auto app (`com.google.android.projection.gearhead`) from an
`.xapk` bundle, which is how it is usually distributed: a base APK plus split APKs for the
CPU architecture, language and screen density.

```bash
cargo install apkeep       # once
dorker run -d --name auto  # fetches the .xapk, builds (~1.5 min), runs it
dorker exec auto "pm list packages | grep gearhead"
dorker rm -f auto
```

The Dorkerfile's `FETCH ./fetch.sh` line makes dorker run `fetch.sh` (which downloads the
latest `.xapk` with apkeep) only when the file is missing, and it asks first because it is a
script on your machine. A file that is already there is never replaced: delete it, or run
`./fetch.sh`, to get a newer one. A new file has a new hash, so the next `dorker build` or
`dorker run` makes a new image.

## The head unit server

Android Auto's developer mode is a setting inside the app, so installing the APK does not
turn it on, and the head unit server (what a head unit emulator connects to) does not run
until it is started. The Dorkerfile does both with a `PROVISION` step:

```dorkerfile
PROVISION ./enable-dev-mode.sh
```

`dorker run` runs the script once, while it builds the image, and the snapshot keeps the
server running. It asks once before running the scripts (they are code on your machine, so read
it first; `--allow-provision` skips the question). The first `dorker run` takes about
1.5 minutes; after that an unchanged
Dorkerfile is a cache hit, and the instance starts in a few seconds with the server already
listening (port 5277 in the guest).

The script drives the app's UI over adb. It finds each button by its text, not by screen
position, and it is safe to run again. It needs `adb` and `python3`, and the app's UI must
be in English. You can also run it by hand on a running instance and save the result:

```bash
./enable-dev-mode.sh setup  # a dorker instance name or an adb serial
dorker commit setup -t gearhead:ready
```

To do it fully by hand: Android Auto has no launcher icon, so open its settings with

```bash
PKG=com.google.android.projection.gearhead
adb -s emulator-5554 shell am start \
  -n $PKG/.companion.settings.DefaultSettingsActivity
```

then scroll to the bottom and tap **Version** about 10 times, choose **OK** in the "Allow
development settings?" dialog, open the **&#8942;** menu and choose **Start head unit server**.

Checked on an Apple Silicon Mac with `android:36-playstore` and Android Auto 17.9.

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
