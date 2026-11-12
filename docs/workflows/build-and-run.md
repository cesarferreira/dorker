# Build and run

`dorker run` with no image starts what the Dorkerfile in the current folder currently says. It builds first. That build is instant when nothing changed, so edit the file and run again to get the new environment.

```bash
dorker versions
dorker run -d --name t1
dorker screenshot t1 shot.png
dorker test t1 \
  --runner com.example.app.test/androidx.test.runner.AndroidJUnitRunner
dorker rm -f t1
```

Use `-f path/to/Dorkerfile` for a file elsewhere. To name the image and reuse it later, split the steps:

```bash
dorker pull
dorker build -t myapp:latest .
dorker run -d --name t1 myapp:latest
```

On a Mac the emulator renders on the GPU by default (`GPU host`), which keeps it smooth when you watch the screen with scrcpy: about 43% CPU while scrolling, against about 600% for software rendering. See [Benchmarks](../benchmarks.md).

## Run scripts while building

Two directives run a script on your machine while an image is built. Both run automatically without a confirmation prompt when needed; the rules below determine when each script is skipped.

```dorkerfile
FROM android:35
INSTALL ./app.xapk
FETCH ./fetch.sh      # creates app.xapk, but only when it is missing
PROVISION ./setup.sh  # runs once, against the build emulator
```

- `FETCH` is for files `INSTALL` needs and you do not commit (a download, for example). It runs only when one of them is missing, never when it is there, so a cached build downloads nothing. Delete the file, or run the script yourself, to refresh it.
- `PROVISION` gets the build emulator's adb serial. Use it for setup that needs the device, such as turning on a developer mode. An unchanged script is not run again.

`dorker build` and `dorker run` run these scripts automatically, including in CI. They execute code on your machine, so only build trusted Dorkerfiles and scripts. The MCP server refuses to run them. The rules are in the [Dorkerfile reference](../dorkerfile.md).

## Start from a template

```bash
dorker templates                             # list them
dorker templates android-auto-with-dev-mode  # what one contains
dorker init --template screenshots           # copy it into this folder
dorker init --template playstore my-app      # or into another folder
```

| Template | What it gives you |
|---|---|
| `basic` | A plain Dorkerfile (the default for `dorker init`) |
| `playstore` | A Google Play image (Play Store app, Google-signed system apps) |
| `instrumentation-tests` | Build a Gradle app and run its instrumentation tests |
| `screenshots` | Install an APK, launch it, save screenshots |
| `android-auto` | Android Auto installed from an `.xapk` |
| `android-auto-with-dev-mode` | The same, with developer mode on and the head unit server running |

`init` never overwrites existing files unless you pass `--force`. How to add a template: [Templates](../templates.md).

### Android Auto with its head unit server running

```bash
cargo install apkeep       # once: downloads the app
dorker init --template android-auto-with-dev-mode
dorker run -d --name auto  # downloads, installs and configures Android Auto
```

`dorker run` downloads the Android Auto `.xapk` (the `FETCH` line), installs it with its split APKs, turns on developer mode and starts the head unit server (the `PROVISION` line), and saves a snapshot. The first run takes about 1.5 minutes:

```text
Fetching missing files (./com.google.android.projection.gearhead.xapk) ...
[2/6] Installing ./com.google.android.projection.gearhead.xapk
[4/6] Persisting provisioned state: flush, reboot, verify (about 40s)
[5/6] Running ./enable-dev-mode.sh
• head unit server is listening (port 5277)
[6/6] Saving snapshot "ready"
Ready in 3.9s (snapshot restore)
```

Every later `dorker run` is a cache hit that starts in a few seconds, with the server already listening. The template uses a Google Play image, because the plain one carries an Android Auto stub that the real app cannot replace. `dorker templates android-auto-with-dev-mode` prints its notes.

Worked examples live in the repository: [examples/basic](https://github.com/cesarferreira/dorker/tree/main/examples/basic), [examples/instrumentation](https://github.com/cesarferreira/dorker/tree/main/examples/instrumentation), and [examples/counter-app](https://github.com/cesarferreira/dorker/tree/main/examples/counter-app), a real Gradle app with Espresso tests (`./dev.sh` runs Gradle, `dorker build`, `run`, a screenshot, and the tests). The example APKs are not committed. Build them with each example's `make-apk.sh`, or with Gradle for the counter app.

## Next

- [Dorkerfile](../dorkerfile.md)
- [Testing](testing.md)
- [Templates](../templates.md)
