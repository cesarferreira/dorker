<div align="center">
  <h1>dorker</h1>

  <p><strong>Docker for headless Android emulator environments</strong></p>

  <p>
    <img alt="License" src="https://img.shields.io/badge/license-MIT-green">
    <img alt="Rust" src="https://img.shields.io/badge/rust-1.85%2B-orange">
    <img alt="Edition" src="https://img.shields.io/badge/edition-2024-blue">
    <a href="https://crates.io/crates/dorker"><img alt="crates.io" src="https://img.shields.io/crates/v/dorker.svg"></a>
  </p>

  <p>
    <a href="#install">Install</a>
    &nbsp;·&nbsp;
    <a href="#quickstart">Quickstart</a>
    &nbsp;·&nbsp;
    <a href="#development">Development</a>
  </p>
</div>

---

## Install

Requires [Rust](https://rustup.rs) **1.85+** and `~/.cargo/bin` on your `PATH`.

```bash
cargo install dorker
```

Verify:

```bash
dorker --help
```

<details>
<summary><strong>Build from source</strong> — for development or unreleased changes</summary>

```bash
git clone https://github.com/cesarferreira/dorker.git
cd dorker
cargo install --path . --locked
# or
make install-release
```

Debug install (faster compile, larger binary):

```bash
make install
```

Run without installing:

```bash
make build-release
./target/release/dorker
```

</details>

<a id="quickstart"></a>
## Quickstart

Requires the Android SDK (`emulator`, `platform-tools`) and an installed system
image (`dorker pull` installs one). macOS Apple Silicon and Linux x86_64 with KVM are
validated. No Docker needed.

```bash
# Check your setup, see which Android versions you can use, install one
dorker doctor
dorker versions                      # installed and available to pull
dorker pull android:35

# Boot a headless emulator and use it
dorker run -d --name test android:35
dorker ps
dorker exec test "getprop ro.build.version.sdk"
dorker install test ./app-debug.apk
dorker screenshot test shot.png      # waits up to 10s for a non-blank frame
dorker logs test                     # add -f to follow

# Stop it, bring it back (cold boot, keeps its data), then delete it
dorker stop test
dorker start test
dorker rm test
```

### Describe your environment in a Dorkerfile

A `Dorkerfile` (next to your app, like a Dockerfile) declares the Android environment: the
base image, resources, the APK to install, settings, and a snapshot to start from.

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

Then build the image once and run it as many times as you like:

```bash
# Not sure which versions exist? List them (installed and available)
dorker versions

# Build the Dorkerfile in this folder (instant if nothing changed) and run it
dorker run -d --name t1
dorker screenshot t1 shot.png
dorker test t1 \
  --runner com.example.app.test/androidx.test.runner.AndroidJUnitRunner
dorker rm -f t1

# Or in the foreground; the instance is removed on Ctrl-C
dorker run --rm
dorker run --logs                    # also stream the device log (logcat)
```

On a Mac the emulator renders on the GPU by default (`GPU host`), which keeps it smooth
when you watch the screen with scrcpy: about 43 % CPU while scrolling against about 600 %
for software rendering. See [docs/benchmarks.md](docs/benchmarks.md).

`dorker run` with no image always starts what the Dorkerfile *currently* says: it builds
first, which is a cache hit when nothing changed, so edit the file and run again to get
the new environment. Use `-f path/to/Dorkerfile` for a file elsewhere. For more control,
the steps are separate commands, and an image can be named and reused:

```bash
dorker pull                              # install the FROM image if missing
dorker build -t myapp:latest .           # APK, settings, snapshot, lockfile
dorker run -d --name t1 myapp:latest     # run it by name, any time
```

If the base image isn't installed, `dorker build` and `dorker run` offer to install
it: on a terminal they ask first (it is a download of 1 GB or more, and Google's
`android` CLI accepts the SDK licences for you). In scripts and CI, where nothing can be
asked, they fail with a hint; pass `--pull` (or set `DORKER_PULL=1`) to install without
asking.

An identical rebuild is a cache hit (instant), and every `dorker run` gets its own isolated
copy, so instances never share state. To try it without your own app, use the sample:

```bash
# Build the tiny sample APK (needs the Android build-tools and a JDK)
examples/basic/make-apk.sh

dorker build -t basic examples/basic
dorker run -d --name b1 basic
dorker exec b1 "pm list packages | grep dorker"
dorker rm -f b1
```

`INSTALL` takes an `.apk` or an `.xapk` bundle (split APKs and OBB files), and
`FROM android:35-playstore` picks the Google Play Store image. Every directive is
documented in [docs/dorkerfile.md](docs/dorkerfile.md). More examples:
[examples/basic](examples/basic), [examples/instrumentation](examples/instrumentation) and
[examples/counter-app](examples/counter-app), a real Gradle app with Espresso tests
(`./dev.sh` runs Gradle, `dorker build`, `run`, a screenshot and the tests). The example APKs
are not committed: build them with each example's `make-apk.sh` (or Gradle for the counter app).

Each instance gets its own private AVD under `~/.dorker` (override with
`DORKER_HOME`). Your existing AVDs are never touched. `dorker run --rm` removes the
instance when it stops. `ps`, `images` and `doctor` accept `--json`.

Measured on an Apple M4 Pro, API 35, 2GB: cold boot about 15s, snapshot restore
about 4s. Host memory is about 2.5GB after a cold boot and 3.3GB after a restore.
Details and caveats: [docs/benchmarks.md](docs/benchmarks.md).

### Keep what you set up by hand

Some state can only be made by using the device: accepting a consent screen, switching on
a developer mode, signing in. Set it up on a running instance, then save it as an image:

```bash
dorker run -d --name setup myapp     # boot, then do the setup (scrcpy, adb)
dorker commit setup -t myapp:ready   # snapshot it and save it as an image
dorker run -d --name t1 myapp:ready  # starts exactly where you left off
```

`commit` saves a snapshot, stops the instance (its files must not change while they are
copied) and copies it into the image store, so `dorker start setup` still brings the
instance back with its data. A new instance restores the saved snapshot in a few seconds,
with the app state that was on screen.

### Configuration

Settings that differ between machines (a Mac and a Linux box, say) go in
`~/.config/dorker/config.toml` (`$XDG_CONFIG_HOME/dorker/config.toml` if that is set;
`DORKER_CONFIG` names another file). Every setting is optional:

```toml
home = "/mnt/big-disk/dorker"   # where images and instances are kept
sdk = "~/Android/Sdk"           # the Android SDK
gpu = "swiftshader_indirect"    # default GPU mode (a GPU line wins)
min_free_mb = 8192              # least free disk needed to start work
```

An environment variable beats the file (`DORKER_HOME`, `ANDROID_HOME`,
`DORKER_MIN_FREE_MB`), and so does a flag or a Dorkerfile line. `dorker config` shows
each setting in effect and where it came from, `dorker config init` writes a commented
example, and a mistake in the file is an error that names it. Keep `home` on one
filesystem: instances are copy-on-write clones of images. `gpu` is part of an image's
identity, so changing it rebuilds images. Things that carry a risk (`--pull`,
`--allow-provision`) are not settable here: they stay a flag or an environment variable.

### Start from a template

```bash
dorker templates                          # list them
dorker templates android-auto-with-dev-mode   # what one contains
dorker init --template screenshots        # copy it into this folder
dorker init --template playstore my-app   # ...or into another folder
```

A template is a Dorkerfile plus any helper scripts. Shipped ones: `basic` (the default),
`playstore`, `instrumentation-tests`, `screenshots`, `android-auto` (the app from an
`.xapk`) and `android-auto-with-dev-mode` (also turns on developer mode and starts the head
unit server). `init` never overwrites existing files unless you pass `--force`. New
templates are welcome as pull requests: see [docs/templates.md](docs/templates.md).

### Run a script while building

`PROVISION ./setup.sh` in a Dorkerfile runs a host script once, against the build emulator
(it gets the adb serial). Use it for setup that needs the device, such as turning on a
developer mode. The result is cached with the image: an unchanged script is not run again.
Because it is code on your machine, `dorker build` / `dorker run` ask before running one
(read it first); in scripts pass `--allow-provision` or set `DORKER_ALLOW_PROVISION=1`.
See [docs/dorkerfile.md](docs/dorkerfile.md) and the `android-auto-with-dev-mode` template.

### Stop everything

```bash
dorker stop --all                   # every running instance
dorker stop --orphans               # emulators dorker lost track of
```

`dorker ps` warns about an orphan. `--orphans` asks before stopping, because such an
emulator may belong to another `DORKER_HOME` that is still using it (`--yes` skips the
question).

### See what is using disk

```bash
dorker ls                          # images, instances and storage (alias: df)
dorker ls --json
```

It lists every image (tags, Android version, size, last used, how many instances use it),
every instance (with its own size) and a storage summary: totals, how much `dorker prune`
could reclaim, free space on the disk and the Android SDK system images dorker does not
manage. On macOS (APFS) an instance shares disk blocks with its image, so its size is only
what it wrote itself; elsewhere instances are full copies and are counted in full.

### Free disk space

Every Dorkerfile change makes a new image of about 3-5 GB. `dorker prune` removes the ones
you no longer need (`dorker ls` shows what that would be):

```bash
dorker prune --dry-run             # show what would be removed
dorker prune                       # remove it, after asking
dorker prune --all --instances     # also tagged images and stopped instances
dorker prune --older-than 7d -y    # only things idle for a week, no prompt
```

By default it removes only images with no tag that no instance was made from; tagged
images, images behind existing instances and running instances are always kept (`--all`
adds unused tagged images, `--instances` adds stopped ones). It lists what it will remove
first and, outside a terminal, refuses to delete without `--yes`. It also clears the
leftovers of interrupted builds. A removed image is simply rebuilt the next time you need it.

### Test, benchmark, automate

```bash
dorker init                          # starter Dorkerfile (--template X)
dorker test t1 \
  --runner com.example.test/androidx.test.runner.AndroidJUnitRunner
dorker test t1 --runner ... --class com.example.FooTest#bar

# Boot, restore, launch, disk and idle numbers
dorker bench myapp:latest --runs 3 --json

# Also time the plain official emulator, for comparison
dorker bench android:35 --baseline
```

`dorker test` prints a summary, preserves the raw instrumentation output and logcat
under the instance directory, and exits 7 if anything failed.
See [examples/instrumentation](examples/instrumentation) for a runnable example.

### Tests

```bash
# Needs cargo-nextest (cargo install cargo-nextest --locked)

# Unit, CLI and fake-SDK lifecycle/failure tests (no emulator needed)
cargo nextest run

# End-to-end: boots real emulators, one at a time (about 3 minutes)
cargo nextest run --profile e2e --run-ignored only
```

### Use it from AI agents

`dorker mcp` is a Model Context Protocol server: an agent can start instances, install apps,
run commands, take screenshots, read logs and run tests, all with structured results.

```bash
claude mcp add dorker -- dorker mcp
```

See [docs/mcp.md](docs/mcp.md) for the tools, other clients and security notes. The same
operations are available to scripts as `--json` on `run`, `start`, `exec`, `install`,
`screenshot`, `test`, `ps`, `images`, `doctor`, `bench`, `versions`, `prune` and `ls`.

### Diagnostics

`doctor`, `ls`, `ps`, `images`, `versions` and `prune` colour their output on a terminal
(green ok, yellow warn, bold red FAIL, fixes and IDs in cyan, sizes in yellow). Colour is off when piped, `NO_COLOR` turns it off, `CLICOLOR_FORCE=1` forces it on,
and `--json` is never coloured.

`dorker -v <command>` (or `DORKER_VERBOSE=1`) prints what dorker decides and runs to stderr: the
located SDK, the boot mode and why, the exact emulator command line and environment, state
transitions and timings. Each instance also keeps its raw emulator output in
`~/.dorker/instances/<id>/emulator.log`.

### Exit codes

| Code | Meaning |
|---|---|
| 0 | success |
| 1 | other error |
| 2 | invalid configuration or arguments (including Dorkerfile errors) |
| 3 | missing dependency (SDK, emulator, adb, system image); `doctor` failures |
| 4 | image build failed |
| 5 | instance failed to start or boot |
| 6 | adb operation failed |
| 7 | instrumentation tests failed |
| 8 | name, port or build conflict, or not enough free disk space |

`dorker exec` returns the guest command's own exit status.

See [docs/architecture.md](docs/architecture.md) for design and limitations.

## Limitations

- **Platform:** validated on macOS Apple Silicon (primary) and Linux x86_64 with KVM (Ubuntu 24.04, an Azure VM with nested virtualisation; unit tests and the full emulator end-to-end suite pass). Linux arm64 and bare-metal Linux are untested; Windows is unsupported. Linux is slower in our measurements (cold boot about 29s, snapshot restore about 9s; see [docs/benchmarks.md](docs/benchmarks.md)).
- **System images are installed on request, not automatically.** `dorker pull android:35` runs Google's `android sdk install` (the replacement for the deprecated `sdkmanager`; older command-line tools fall back to `sdkmanager`). The new CLI accepts the Android SDK licences (and the ARM translation licence for arm64 images) for you without prompting, so `pull` says so first, and Google documents that it collects usage data (opt out with `android --no-metrics`); `dorker run`/`build` print this command when an image is missing. The download path itself was only verified for the already-installed and nonexistent-package cases, not a real multi-GB download.
- **Snapshots are strict.** A change of emulator version, system-image revision, memory or CPUs invalidates an image's snapshot. dorker then cold boots (about 15s) and says so. Snapshots are host-specific and not portable between machines.
- **Memory:** a 2GB guest used about 2.5GB of host memory after a cold boot and 3.3GB after a snapshot restore on macOS (3-3.7GB on Linux). Idle CPU is dominated by Android background work and takes about a minute to settle.
- **Builds take about 45s** because dorker reboots the guest to persist provisioned state.
- **`CMD`** only supports `launch <package>`. `dorker start` always cold boots (about 14s) because restoring the image snapshot would roll back the instance's data. Rendering uses the Mac GPU (`host`) on macOS and software rendering (`swiftshader_indirect`) elsewhere; `GPU <mode>` in the Dorkerfile selects another. No mode was faster for snapshot restore in our measurements (see docs/benchmarks.md). There is no automatic test-APK discovery.
- **Concurrency:** creating instances concurrently is safe, and `stop`, `rm` and `start` on the same instance are serialised by a per-instance lock (the second command waits up to 60s, then exits 8).
- **Orphans:** emulator processes that dorker started but lost track of (CLI killed mid-start) are reported by `dorker ps`, not adopted or killed automatically.
- `examples/basic` and `examples/instrumentation` use tiny hand-built APKs; `examples/counter-app` is the realistic one.

<a id="development"></a>
## Development

Common tasks via the `Makefile`:

```bash
make              # check + build + test
make build        # debug build
make build-release
make install      # install debug binary
make install-release
make run ARGS="doctor"
make check        # cargo check + clippy
make fmt          # format
make lint         # fmt check + clippy
make test
make clean
make demo         # install + show --help
```

Releasing (requires [cargo-release](https://github.com/crate-ci/cargo-release) and [git-cliff](https://github.com/orhun/git-cliff)):

```bash
make release                  # default minor bump
make release LEVEL=patch      # patch bump
make release LEVEL=major      # major bump
```

The pre-release hook regenerates `CHANGELOG.md` with `git-cliff` from your conventional-commit history (grouped into Features, Bug Fixes, etc. per `cliff.toml`) and commits it alongside the version bump. Pushing the resulting `v*` tag triggers the release workflow, which builds the multi-platform binaries and publishes a GitHub Release whose notes are generated by `git-cliff` from the same config.

## License

MIT
