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
dorker doctor                                   # check your setup
dorker pull android:35                          # install a system image if missing
dorker run -d --name test android:35            # boots a headless emulator
dorker ps
dorker exec test "getprop ro.build.version.sdk"
dorker install test ./app-debug.apk
dorker screenshot test shot.png
dorker logs test                                # add -f to follow
dorker stop test
dorker start test                               # cold boots again, keeps its data
dorker rm test
```

### Build an image

```bash
dorker build -t myapp:latest .          # reads ./Dorkerfile, writes Dorkerfile.lock
dorker images
dorker run -d --name t1 myapp:latest    # restores the snapshot in a few seconds
dorker rm -f t1
```

See [docs/dorkerfile.md](docs/dorkerfile.md) and [examples/basic](examples/basic).
[examples/counter-app](examples/counter-app) is a real Gradle app with Espresso tests
(`./dev.sh` runs Gradle, `dorker build`, `run`, a screenshot and the tests).
The example APKs are not committed: run `examples/basic/make-apk.sh` first (needs the
Android build-tools and a JDK).

Each instance gets its own private AVD under `~/.dorker` (override with
`DORKER_HOME`). Your existing AVDs are never touched. `dorker run --rm` removes the
instance when it stops. `ps`, `images` and `doctor` accept `--json`.

Measured on an Apple M4 Pro, API 35, 2GB: cold boot about 15s, snapshot restore
about 4s. Host memory is about 2.5GB after a cold boot and 3.3GB after a restore.
Details and caveats: [docs/benchmarks.md](docs/benchmarks.md).

### Test, benchmark, automate

```bash
dorker init                                   # starter Dorkerfile
dorker test t1 --runner com.example.test/androidx.test.runner.AndroidJUnitRunner
dorker test t1 --runner ... --class com.example.FooTest#bar
dorker bench myapp:latest --runs 3 --json     # boot, restore, launch, disk, idle usage
dorker bench android:35 --baseline            # also time the plain official emulator
```

`dorker test` prints a summary, preserves the raw instrumentation output and logcat
under the instance directory, and exits 7 if anything failed.
See [examples/instrumentation](examples/instrumentation) for a runnable example.

### Tests

```bash
cargo test                          # unit + CLI tests, no emulator needed
cargo test -- --ignored --test-threads=1   # end-to-end: boots real emulators (~2 min)
```

### Diagnostics

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
- **System images are installed on request, not automatically.** `dorker pull android:35` runs the SDK's `sdkmanager` (needs the command-line tools and may prompt for licences); `dorker run`/`build` print this command when an image is missing. The download path itself was only verified for the already-installed and nonexistent-package cases, not a real multi-GB download.
- **Snapshots are strict.** A change of emulator version, system-image revision, memory or CPUs invalidates an image's snapshot. dorker then cold boots (about 15s) and says so. Snapshots are host-specific and not portable between machines.
- **Memory:** a 2GB guest used about 2.5GB of host memory after a cold boot and 3.3GB after a snapshot restore on macOS (3-3.7GB on Linux). Idle CPU is dominated by Android background work and takes about a minute to settle.
- **Builds take about 45s** because dorker reboots the guest to persist provisioned state.
- **`CMD`** only supports `launch <package>`. `dorker start` always cold boots (about 14s) because restoring the image snapshot would roll back the instance's data. There is software rendering (`swiftshader_indirect`) is the default; `GPU <mode>` in the Dorkerfile selects another, but none was faster for snapshot restore in our measurements (see docs/benchmarks.md) and no automatic test-APK discovery.
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
