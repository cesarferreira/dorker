---
name: dorker
description: Use when a task needs a headless Android emulator - running or screenshotting an app, UI or instrumentation tests, sharding tests across emulators, CI device setup, a Dorkerfile, or the `dorker` CLI.
---

# dorker

Docker for headless Android emulators. A `Dorkerfile` is built once into an immutable image (with a snapshot, so it boots in seconds). Each `dorker run` is an isolated, disposable instance. Needs the Android SDK; no Docker. Run `dorker doctor` first if anything is missing.

## Quick Reference

| Goal | Command |
|---|---|
| Check setup | `dorker doctor` |
| Android versions | `dorker versions`, `dorker pull android:35` |
| Start a Dorkerfile project | `dorker init --template <name>` (`dorker templates` lists them) |
| Build + run the Dorkerfile in cwd | `dorker run -d --name t1` |
| Build a named image | `dorker build -t myapp:latest .` |
| Run a base or built image | `dorker run -d --name t1 android:35` |
| List instances | `dorker ps` (`-a` includes stopped) |
| Shell command on device | `dorker exec t1 "pm list packages"` |
| Install APK | `dorker install t1 app.apk` |
| Screenshot | `dorker screenshot t1 shot.png` (`-` = PNG on stdout) |
| Logcat | `dorker logs t1` (`-f` follow, `--tail N`) |
| Instrumentation tests | `dorker test t1 --runner <pkg.test>/androidx.test.runner.AndroidJUnitRunner` |
| Parallel shards | `dorker test --image myapp:ci --workers 4 --runner ...` |
| Save hand-made device state | `dorker commit t1 -t myapp:ready` |
| Stop / delete | `dorker stop t1`, `dorker rm -f t1`, `dorker stop --all` |
| Disk | `dorker ls`, `dorker prune --dry-run` |

Most commands take `--json`. `-v` (or `DORKER_VERBOSE=1`) prints what dorker decides and runs, on stderr.

## Dorkerfile

```dorkerfile
FROM android:35            # android:35-playstore for the Play Store image
MEMORY 2GB
CPUS 2
INSTALL ./app/build/outputs/apk/debug/app-debug.apk   # .apk or .xapk
SETTING window_animation_scale 0
SNAPSHOT ready
CMD launch com.example.app
```

Other directives: `DISK`, `DEVICE`, `RESOLUTION`, `HEADLESS`, `GPU`, `FETCH <script>` (creates missing `INSTALL` files), `PROVISION <script>` (runs once against the build emulator). `FETCH` and `PROVISION` run code on the host: build only trusted Dorkerfiles. Paths are relative to the Dorkerfile. Run `dorker run` with no image to rebuild (cache hit if unchanged) and start the current file.

## Rules

- Name instances (`--name`) and `rm -f` them when done; `run` without `-d` stays attached and `--rm` removes on stop.
- Scripts and CI cannot answer prompts: pass `--pull` (or `DORKER_PULL=1`) to install a missing system image, `--yes` to `prune` and `stop --orphans`.
- `dorker start` always cold boots (about 14s) and keeps the instance's data; a new `run` restores the snapshot (about 4s).
- `dorker test` keeps logs inside the instance; use `--out <dir>` to keep them past `rm`. With `--image`, results go to `~/.dorker/results/<run-id>/` (`summary.json`, `junit.xml`).
- Put `DORKER_HOME` on a filesystem that can clone (APFS, XFS reflink, Btrfs), or every instance is a full multi-GB copy.
- `dorker mcp` is an MCP server (`claude mcp add dorker -- dorker mcp`); it refuses `FETCH`/`PROVISION`.

## Exit Codes

| Code | Meaning |
|---|---|
| 2 | invalid config, arguments or Dorkerfile |
| 3 | missing SDK, emulator, adb or system image |
| 4 | image build failed |
| 5 | instance failed to start or boot |
| 6 | adb operation failed |
| 7 | tests failed or sharded run incomplete |
| 8 | name, port or build conflict, low disk, or `--workers` do not fit (`--force` overrides the capacity check) |
| 130 | `test --image` interrupted |

`dorker exec` returns the guest command's own exit status.

Run `dorker <command> --help` for flags.
