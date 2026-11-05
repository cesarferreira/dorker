# Changelog

All notable changes to this project will be documented in this file.

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
