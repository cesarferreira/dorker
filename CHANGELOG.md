# Changelog

All notable changes to this project will be documented in this file.

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
