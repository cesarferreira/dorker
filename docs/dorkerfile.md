# Dorkerfile reference

A Dorkerfile describes an Android environment. Directives are case-insensitive,
`#` starts a comment, and values may be quoted (`INSTALL "my app/app.apk"`).
`FROM` must come first. Duplicate singleton directives are rejected. Errors
include `file:line:col` and, for typos, a suggestion.

```dockerfile
FROM android:35

MEMORY 2GB
CPUS 2
DISK 4GB

DEVICE pixel_7
RESOLUTION 1080x2400
HEADLESS true

INSTALL ./app/build/outputs/apk/debug/app-debug.apk

SETTING window_animation_scale 0
SETTING secure show_ime_with_hard_keyboard 0

SNAPSHOT ready
CMD launch com.example.application
```

| Directive | Arguments | Notes |
|---|---|---|
| `FROM` | `android:<api>[-<flavour>]` | Resolved against **installed** system images for the host ABI. Without a flavour, `google_apis` is preferred, then `default`, then others |
| `MEMORY` | `2GB`, `2048MB` | 1GB-32GB. Default 2GB |
| `CPUS` | integer | 1-16, at most the host's cores. Default 2 |
| `DISK` | `4GB` | Userdata partition size, 2GB-64GB. Default 4GB |
| `DEVICE` | profile name | Written to the AVD (`hw.device.name`). Default `pixel_7` |
| `RESOLUTION` | `WxH` | Default `1080x2400` |
| `HEADLESS` | `true`/`false` | Default `true` |
| `INSTALL` | path | Repeatable. Relative to the build context; paths that escape it (`..`, symlinks) are rejected |
| `SETTING` | `[global\|secure\|system] key value` | Repeatable. Namespace defaults to `global`. Only `[A-Za-z0-9_.:-]` is accepted |
| `SNAPSHOT` | name | Save an emulator snapshot after provisioning. Instances restore it in a few seconds |
| `CMD` | `launch <package>` | Launches the package's launcher activity after the instance is ready. Other forms are rejected |

Arbitrary host commands are not supported, by design.

## Builds and the lockfile

`dorker build [-t repo:tag] [-f Dorkerfile] [context]` boots a build instance,
applies the directives in order, saves the snapshot, and publishes an immutable
image. The image id is a fingerprint of the system-image revision, emulator
version, resources, APK **contents**, settings and snapshot name, so an identical
rebuild is a cache hit (`--no-cache` forces a rebuild).

`Dorkerfile.lock` is written next to the Dorkerfile and records the resolved
system image and revision, emulator version, device configuration and APK
SHA-256 hashes. dorker warns when the installed system image or emulator no
longer matches the lock. It does not download anything, so the lock pins what
was resolved rather than fetching it.

## Snapshots and compatibility

A snapshot is only restored when the hardware configuration, system-image
revision and emulator version match the image. Otherwise `dorker run` prints a
note and cold boots. Overriding `--memory` or `--cpus` therefore disables the
snapshot.
