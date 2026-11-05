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
image. macOS Apple Silicon is the supported platform. No Docker needed.

```bash
dorker doctor                                   # check your setup
dorker run -d --name test android:35            # boots a headless emulator
dorker ps
dorker exec test "getprop ro.build.version.sdk"
dorker install test ./app-debug.apk
dorker screenshot test shot.png
dorker logs test                                # add -f to follow
dorker stop test
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

Each instance gets its own private AVD under `~/.dorker` (override with
`DORKER_HOME`). Your existing AVDs are never touched. `dorker run --rm` removes the
instance when it stops. `ps`, `images` and `doctor` accept `--json`.

Measured on an Apple M4 Pro, API 35, 2GB: cold boot about 15s, snapshot restore
about 4s. A 2GB guest used about 5.5GB of host memory. Details and caveats:
[docs/benchmarks.md](docs/benchmarks.md).

### Test, benchmark, automate

```bash
dorker init                                   # starter Dorkerfile
dorker test t1 --runner com.example.test/androidx.test.runner.AndroidJUnitRunner
dorker test t1 --runner ... --class com.example.FooTest#bar
dorker bench myapp:latest --runs 3 --json     # boot, restore, install, idle usage
```

`dorker test` prints a summary, preserves the raw instrumentation output and logcat
under the instance directory, and exits 7 if anything failed.
See [examples/instrumentation](examples/instrumentation) for a runnable example.

### Tests

```bash
cargo test                          # unit + CLI tests, no emulator needed
cargo test -- --ignored --test-threads=1   # end-to-end: boots real emulators (~2 min)
```

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
| 8 | name, port or build conflict |

`dorker exec` returns the guest command's own exit status.

See [docs/architecture.md](docs/architecture.md) for design and limitations.

## Limitations

- **Platform:** validated on macOS Apple Silicon only. Linux may work but is untested; Windows is unsupported.
- **System images are not downloaded.** Install them with `sdkmanager`; `dorker doctor` and the error messages print the exact command.
- **Snapshots are strict.** A change of emulator version, system-image revision, memory or CPUs invalidates an image's snapshot. dorker then cold boots (about 15s) and says so. Snapshots are host-specific and not portable between machines.
- **Memory:** a 2GB guest used about 5.5GB of host RAM in our measurements.
- **Builds take about 45s** because dorker reboots the guest to persist provisioned state.
- **`CMD`** only supports `launch <package>`. There is no `dorker start` for stopped instances yet, no GPU-mode selection (software rendering is used when headless) and no automatic test-APK discovery.
- The manifest-only sample APKs in `examples/` are for demonstration, not real apps.

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
