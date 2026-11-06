# Images and instances

A **Dorkerfile** describes an Android environment. `dorker build` turns that description into an **image**: an immutable copy of a provisioned emulator, including a snapshot. `dorker run` starts an **instance**, which is an isolated copy of that image.

```bash
dorker pull                           # install the FROM image if missing
dorker build -t myapp:latest .        # APK, settings, snapshot, lockfile
dorker run -d --name t1 myapp:latest  # run it by name, any time
```

An identical rebuild is a cache hit (instant). Every `dorker run` gets its own copy, so instances never share state. `dorker run --rm` removes the instance when it stops.

If the base image is not installed, `dorker build` and `dorker run` offer to install it. On a terminal they ask first: it is a download of 1 GB or more, and Google's `android` CLI accepts the SDK licences for you. In scripts and CI, where nothing can be asked, they fail with a hint. Pass `--pull` (or set `DORKER_PULL=1`) to install without asking.

## What lives where

Each instance gets its own private AVD under `~/.dorker` (see [Configuration](../configuration/index.md) to move it). Your existing AVDs are never touched.

| Piece | What it is |
|---|---|
| System image | An Android SDK image, such as `android:35`. `dorker pull` installs it. |
| Image | The built result of a Dorkerfile: APKs, settings, and a snapshot. Named with `-t`, for example `myapp:latest`. |
| Instance | A running (or stopped) copy of an image. `dorker run -d --name t1` creates one. |
| Snapshot | Saved emulator state inside the image. A new instance restores it in a few seconds. |

`dorker start` always cold boots (about 14s) because restoring the image snapshot would roll back the instance's data. A snapshot restore is for a **new** instance of the image.

## Save a setup you did by hand

Some state can only be made by using the device: accepting a consent screen, switching on a developer mode, signing in. Set it up on a running instance, then save it as an image:

```bash
dorker run -d --name setup myapp     # boot, then do the setup (scrcpy, adb)
dorker commit setup -t myapp:ready   # snapshot it and save it as an image
dorker run -d --name t1 myapp:ready  # starts exactly where you left off
```

`commit` saves a snapshot, stops the instance (its files must not change while they are copied), and copies it into the image store. `dorker start setup` still brings that instance back with its data. A new instance restores the saved snapshot in a few seconds, with the app state that was on screen.

## Snapshots are strict

A change of emulator version, system-image revision, memory, or CPUs invalidates an image's snapshot. dorker then cold boots (about 15s) and says so. Overriding `--memory` or `--cpus` on `dorker run` therefore disables the snapshot. Snapshots are host-specific and are not portable between machines.

## Copies and disk

A new instance is a clone of its image: `cp -c` on macOS (APFS) and `cp --reflink=auto` on Linux. That is a reflink on XFS created with `reflink=1`, and on Btrfs. The instance shares the image's blocks until it writes, so the copy finishes in milliseconds.

NFS and ext4 cannot clone. dorker then copies the whole image (several GB) for every instance. `~/.dorker` is on your home directory, which is often a network mount. Point `home` or `DORKER_HOME` at a local disk before running several workers. The difference is measured in [Benchmarks](../benchmarks.md#parallel-instrumentation).

Measured on an Apple M4 Pro, API 35, 2GB: cold boot about 15s, snapshot restore about 4s. Host memory is about 2.5GB after a cold boot and 3.3GB after a restore. Details and caveats are in [Benchmarks](../benchmarks.md).

Most commands accept `--json`.

Next: [Dorkerfile →](../dorkerfile.md)
