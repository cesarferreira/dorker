# Changelog

All notable changes to this project will be documented in this file.

## [0.1.7] - 2026-11-06

### 🚀 Features

- Add the PROVISION directive, and stop --all / --orphans
- Add project templates (dorker templates, dorker init --template)
- Add a config file (~/.config/dorker/config.toml) and dorker config
- Add the FETCH directive; the android-auto templates fetch their own .xapk

### 🐛 Bug Fixes

- PROVISION needs consent before it runs host scripts

### 💼 Other

- Run tests with cargo-nextest, locally and in CI

### 📚 Documentation

- Mention enable-dev-mode.sh in the main README's example list
- Reorganise the README, fix stale statements, add the Android Auto walkthrough
- Make the FETCH/PROVISION README example a complete Dorkerfile
- Bring architecture.md up to date
- Add a Dorkerfile screenshot under the README badges
## [0.1.6] - 2026-11-05

### 🚀 Features

- Install .xapk bundles and add the playstore flavour shortcut
- Number the build steps ([2/6] ...) so long builds show their progress
- Add dorker commit to save a running instance as an image
- Add enable-dev-mode.sh to the android-auto example

### 🐛 Bug Fixes

- Validate .xapk OBB install paths before using them in a device shell command
- Enforce the .xapk unpack limit on bytes actually written
- Android-auto setup steps build the Dorkerfile first; hint at it when an image is missing

### 📚 Documentation

- Add an Android Auto example that installs an .xapk fetched with apkeep
- Keep the android-auto example's code block within 78 columns
- Android-auto head unit setup steps, checked on an emulator
- Keep the android-auto setup commands within 78 columns
- Keep the android-auto README code within 78 columns
- Drop the aa mention from the android-auto README and align its code comments
## [0.1.5] - 2026-11-05

### 🚀 Features

- Add dorker versions to list the Android versions you can use
- Build and run offer to install a missing system image
- Dorker run with no image builds and runs the Dorkerfile in this folder
- Add dorker prune to reclaim disk from unused images and stopped instances
- Add dorker ls to show images, instances and disk use
- Colour ls, ps, images and prune output
- Run --logs streams device log; instrumentation example gets a launcher app and host GPU
- Default to host GPU on macOS

### 🐛 Bug Fixes

- Dorker pull reads the published package names instead of guessing them
## [0.1.4] - 2026-11-05

### 🚀 Features

- Dorker pull uses the Android CLI instead of the deprecated sdkmanager
- Colour the doctor report

### 📚 Documentation

- Fix the README code blocks that overflowed and were mis-coloured

### 🧪 Testing

- Make the doctor colour test independent of the runner's SDK
## [0.1.3] - 2026-11-05

### 🚀 Features

- Dorker pull with no arguments installs the Dorkerfile's FROM image

### 🐛 Bug Fixes

- Make screenshots wait for a non-blank frame
## [0.1.2] - 2026-11-05

### 🚀 Features

- Add an MCP server and --json output for agent workflows

### 🐛 Bug Fixes

- Take MCP screenshots over stdout instead of a temporary file

### 📚 Documentation

- HEADLESS false verified on macOS; partial explanation of the restore gap
- Record that the emulator listens on loopback only (macOS)

### 🧪 Testing

- Cover lifecycle and failure paths against a fake Android SDK
## [0.1.1] - 2026-11-05

### 🚀 Features

- Add --verbose diagnostics
- Check free disk space up front and report why an image copy failed
- Bench measures baselines, disk usage and app launch time

### 📚 Documentation

- Generate the changelog with git-cliff
## [0.1.0] - 2026-11-05

### 🚀 Features

- Headless emulator runtime with doctor, run, ps, exec, install, screenshot, logs, stop, rm
- Dorkerfile builds, immutable images and snapshot restore
- Dorker test, bench and init with stable exit codes
- Detect orphaned emulators and stale records, clearer test start failures
- Add a real Gradle example app with Espresso tests
- Dorker start and per-instance locking
- Dorker pull installs system images via sdkmanager
- GPU directive, and make bench measure idle usage honestly

### 🐛 Bug Fixes

- Persist provisioned state before snapshotting and fall back to cold boot
- Wait for the app's first frame after CMD launch
- Measure idle CPU over an interval on Linux
- Harden process identity and support minor Android versions

### 📚 Documentation

- Document current limitations
- Record Linux x86_64 validation, benchmarks and the unexplained startup flake
- GPU-mode and idle-usage measurements, and correct earlier numbers

### ⚙️ Miscellaneous Tasks

- Run tests on Linux only for now
