# Quick start

Requires the Android SDK (`emulator`, `platform-tools`) and an installed system image. `dorker pull` installs one. macOS Apple Silicon and Linux x86_64 with KVM are validated. No Docker is needed.

## 1. Check the host and install an image

```bash
dorker doctor
dorker versions                  # installed and available to pull
dorker pull android:35
```

## 2. Boot a headless emulator

```bash
dorker run -d --name test android:35
dorker ps
dorker exec test "getprop ro.build.version.sdk"
dorker install test ./app-debug.apk
dorker screenshot test shot.png  # waits up to 10s for a non-blank frame
dorker logs test                 # add -f to follow

# Stop it, bring it back (cold boot, keeps its data), then delete it
dorker stop test
dorker start test
dorker rm test
```

## 3. Describe the environment in a Dorkerfile

A `Dorkerfile` (next to your app, like a Dockerfile) declares the Android environment: the base image, resources, the APK to install, settings, and a snapshot to start from.

```dorkerfile
# Dorkerfile
FROM android:35

MEMORY 2GB
CPUS 2
DISK 4GB
DEVICE pixel_7
RESOLUTION 1080x2400
HEADLESS true

# Paths are relative to this file's folder.
INSTALL ./app/build/outputs/apk/debug/app-debug.apk

# Turn off animations: faster, less flaky UI tests.
SETTING window_animation_scale 0
SETTING transition_animation_scale 0
SETTING animator_duration_scale 0

# Save a snapshot so instances start in a few seconds,
# and open the app once an instance is ready.
SNAPSHOT ready
CMD launch com.example.app
```

Build the image once and run it as many times as you like:

```bash
dorker run -d --name t1
dorker screenshot t1 shot.png
dorker rm -f t1

# Or in the foreground; the instance is removed on Ctrl-C
dorker run --rm
dorker run --logs  # also stream the device log (logcat)
```

`dorker run` with no image always starts what the Dorkerfile currently says. It builds first, and that build is a cache hit when nothing changed, so edit the file and run again to get the new environment. Use `-f path/to/Dorkerfile` for a file elsewhere.

To try it without your own app, build the sample APK (it needs the Android build-tools and a JDK) and run that image:

```bash
examples/basic/make-apk.sh
dorker build -t basic examples/basic
dorker run -d --name b1 basic
dorker exec b1 "pm list packages | grep dorker"
dorker rm -f b1
```

`INSTALL` takes an `.apk` or an `.xapk` bundle (split APKs and OBB files). `FROM android:35-playstore` picks the Google Play Store image. Every directive is in the [Dorkerfile reference](../dorkerfile.md).

## Next

- [Images and instances](../concepts/images-and-instances.md)
- [Dorkerfile](../dorkerfile.md)
- [Core commands](../commands/core.md)
