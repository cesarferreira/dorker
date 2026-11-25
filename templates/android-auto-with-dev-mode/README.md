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
latest `.xapk` with apkeep) only when the file is missing, without a confirmation prompt.
It runs code on your machine, so read it before building. A file already there is never replaced: delete it, or run
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
server running. Scripts run automatically without a confirmation prompt, so only build trusted
Dorkerfiles and scripts. The first `dorker run` takes about 1.5 minutes; after that an unchanged
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

Checked on an Apple Silicon Mac with `android:36-playstore` and Android Auto 17.9, and on
x86_64 Linux with `android:37-playstore` and Android Auto 18.0.

Notes:

- It uses `FROM android:36-playstore` on purpose. The plain `google_apis` image carries an
  Android Auto *stub* signed with the AOSP test key, and the real app (signed by Google)
  cannot replace it: `INSTALL_FAILED_UPDATE_INCOMPATIBLE`. The Play Store image's stub is
  signed by Google, so the update installs.
- The bundle from apkeep is for **arm64** (it has `config.arm64_v8a.apk`). It installs on
  Apple Silicon, and also on x86_64 Linux with `android:37-playstore`, whose system image
  translates arm64 code.
- The `google_apis` image already has Google Play services, which Android Auto needs.
  Installing the app does not connect a car: use Android Auto's developer mode and a head
  unit (or the Desktop Head Unit) for that.
- The `.xapk` is not committed (Google's app, version changes). Check that your use of it
  complies with its terms.

## Publish it for your team

`publish.sh` builds this folder with the latest Android Auto and pushes it to a registry.
Run the same command on an Apple Silicon Mac and on an x86_64 Linux box, at the same time
if you like; each adds its own CPU architecture to the tags:

```bash
gh auth refresh -s write:packages   # once, for ghcr.io
./publish.sh ghcr.io/your-org/gearhead --api 37
```

It:

1. sets `FROM android:37-playstore` (without `--api` it keeps the Dorkerfile's `FROM`);
2. downloads the latest Android Auto with apkeep and reads its version, say 18.1.x;
3. stops there if `gearhead:37-aa18.1` already has this machine's architecture (`--force`
   redoes it);
4. **updates the previous version**, the way a car or phone gets Android Auto from the Play
   Store: it runs the newest older image for this architecture (say `gearhead:37-aa18.0`),
   installs the new Android Auto over it (app data and developer mode are kept), starts the
   head unit server and saves the result with `dorker commit`. With `--fresh`, or when there
   is no older version yet, it builds the Dorkerfile from scratch instead (a clean install);
5. checks the result: the head unit server listens on port 5277, and the installed Android
   Auto is the downloaded version. It does not push an image that fails;
6. pushes `gearhead:37-aa18.1`, and moves `gearhead:37` to it when no newer Android Auto
   version is published (so a machine that downloaded an older version cannot move it back);
7. prints which architectures each tag has, and says when the other machine still has to run.

Each version is then the previous one plus an update, on both machines, so `:37-aa18.1` on a
Mac and on Linux have the same history.

Then everyone runs `dorker run -d --name aa ghcr.io/your-org/gearhead:37-aa18.1` (or `:37`
for the newest). See [Share images with your team](../../README.md#share-images-with-your-team).
