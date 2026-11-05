# dorker — Product Requirements Document

**Version:** 1.0  
**Status:** Ready for implementation planning  
**Language:** Rust  
**Initial platform:** macOS Apple Silicon (ARM64)  
**Future platforms:** Linux (ARM64/x86_64)  
**Product category:** Developer tooling / Android virtualisation / CI infrastructure

---

## 1. Executive Summary

**dorker is Docker for Android development environments.**

It is a lightweight, Rust-based command-line application that enables developers to declaratively define, build, run, and destroy headless Android virtual devices.

Instead of manually configuring Android Virtual Devices (AVDs), installing APKs, managing emulator processes, and coordinating ADB connections, developers define their environment in a `Dorkerfile`.

```dockerfile
FROM android:35

MEMORY 2GB
CPUS 2
DISK 4GB

DEVICE pixel_7
HEADLESS true

INSTALL ./build/app-debug.apk

SNAPSHOT ready
```

Then run:

```bash
dorker build -t myapp:latest .
dorker run -d --name test myapp:latest

dorker exec test "pm list packages"
dorker screenshot test screenshot.png

dorker rm -f test
```

The core philosophy is:

> Android environments should be as reproducible, disposable, and easy to operate as Docker containers.

dorker must **not implement a new Android emulator**.

Instead, it should orchestrate existing Android Emulator technology, optimising developer experience, provisioning, startup, image reuse, and resource management.

### Primary objectives

1. Provide reproducible Android environments defined in version-controlled files.
2. Run Android headlessly on Apple Silicon without requiring Docker Desktop.
3. Support reusable, preconfigured Android images.
4. Simplify application installation, execution, instrumentation and automation.
5. Provide isolated, disposable Android instances.
6. Reduce repeated environment setup and provisioning overhead.
7. Support future CI integration and AI-agent workflows.

---

## 2. Required Research Before Implementation

### 2.1 Google Android Emulator Container Scripts

**Repository:**

https://github.com/google/android-emulator-container-scripts

This project MUST be investigated before implementing dorker's image management and emulator orchestration.

Google's repository includes:

- Android Emulator packaging into Docker images.
- Android system-image discovery and downloads.
- Headless emulator execution.
- ADB connectivity.
- Docker image generation and registries.
- GPU acceleration configuration.
- WebRTC-based streaming and remote interaction.
- Linux/KVM-based execution.

Its Docker execution workflow currently targets Linux with KVM. Docker Desktop on macOS does not provide the required KVM acceleration.

**Research tasks for the implementation agent:**

1. Read the README, architecture, source code, and licensing information.
2. Identify reusable techniques for discovering Android SDK images and emulator releases.
3. Examine how system images, writable userdata and emulator configurations are managed.
4. Investigate image creation and provisioning.
5. Study its ADB and gRPC integration.
6. Investigate whether the project's existing tooling could serve as an optional Linux backend.
7. Identify what can be reused conceptually rather than reimplemented.
8. Document which capabilities dorker would genuinely add.

Do not assume Google's project lacks functionality merely because it is absent from introductory documentation.

### 2.2 Additional research references

- Android Emulator documentation: https://developer.android.com/studio/run/emulator
- Android SDK command-line tools: https://developer.android.com/tools
- Android Emulator command-line options: https://developer.android.com/studio/run/emulator-commandline
- Android Debug Bridge: https://developer.android.com/tools/adb
- Apple Virtualization framework: https://developer.apple.com/documentation/virtualization
- QEMU: https://www.qemu.org/
- Android Open Source Project: https://source.android.com/

Investigate current emulator snapshot capabilities, ARM64 support, image compatibility and host resource accounting.

### 2.3 Research deliverable

Before implementation, produce `docs/research.md` documenting:

- Existing solutions and competing tools.
- Functionality that can be reused.
- Confirmed macOS ARM64 limitations.
- Proposed emulator backend.
- Snapshot and disk-overlay feasibility.
- Licensing considerations.
- Technical risks.

Prefer existing documented interfaces over custom implementations.

---

## 3. Problem Statement

Android development environments are unnecessarily complicated to reproduce and automate.

A typical developer needs to:

1. Install Android SDK components.
2. Download a system image.
3. Create and configure an AVD.
4. Allocate memory, CPU cores and storage.
5. Start an emulator.
6. Wait for Android to complete booting.
7. Install application packages.
8. Configure settings, permissions and test prerequisites.
9. Execute tests or other commands.
10. Stop the emulator and manage its persistent state.

These operations are often repeated across developers, projects and CI systems.

Furthermore, Android environment configuration is commonly stored outside application repositories.

This makes it difficult to recreate an identical test environment.

### Proposed solution

dorker introduces:

- A declarative `Dorkerfile`.
- Immutable logical images.
- Reusable prepared instances.
- A standard CLI.
- Managed emulator processes.
- Automated ADB coordination.
- Snapshot and provisioning caches.
- Disposable instances with isolated writable state.

---

## 4. Target Users

### Primary: Android developers

Developers who want to run applications and tests without manually managing Android Studio or AVD configurations.

### Secondary: CI/CD systems

Teams requiring repeatable, automated Android testing environments.

### Tertiary: AI coding agents

Agents such as Claude Code, Codex or other autonomous development tools that need to:

- Launch an Android environment.
- Install an application.
- Execute commands.
- Interact with the application.
- Capture screenshots.
- Inspect logs.
- Run automated tests.
- Tear down the instance.

### Future: Android platform and automotive developers

Potential future support for Android Automotive OS, Android Auto testing infrastructure and specialised Android images.

These are not MVP requirements.

---

## 5. Product Principles

### 5.1 Docker-like experience

Commands and concepts should feel familiar to Docker users.

### 5.2 No unnecessary emulation reinvention

Use the official Android Emulator initially.

A custom virtual machine monitor is explicitly out of scope.

### 5.3 Native execution

macOS support must not require Docker Desktop, a Linux VM, or nested virtualisation.

### 5.4 Reproducibility

Builds should resolve exact Android system-image revisions, emulator versions and APK checksums.

A lockfile should record resolved dependencies.

### 5.5 Disposable by default

Containers should never unintentionally share writable Android userdata.

### 5.6 Fast repeated operations

Optimise repeated builds, app provisioning and instance startup.

### 5.7 Minimal dependencies

The CLI should be a standalone Rust binary where practical.

The Android SDK/emulator and ADB are permitted runtime dependencies.

### 5.8 Measurable improvements

Every claimed performance improvement must be backed by benchmarks.

---

# 6. Dorkerfile Specification

## 6.1 Overview

The `Dorkerfile` defines an Android environment.

Example:

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
SETTING transition_animation_scale 0
SETTING animator_duration_scale 0

SNAPSHOT ready

CMD launch com.example.application
```

This is the proposed syntax. It must be validated against the selected emulator backend before individual directives are considered supported.

## 6.2 Required directives

| Directive | Description |
|---|---|
| `FROM` | Android API level and system-image flavour |
| `MEMORY` | Guest RAM allocation |
| `CPUS` | Virtual CPU count |
| `DISK` | Writable disk allocation |
| `DEVICE` | Device profile |
| `RESOLUTION` | Virtual display resolution |
| `HEADLESS` | Whether to launch without a native display window |
| `INSTALL` | Install an APK into the environment |
| `SETTING` | Apply a supported Android setting |
| `SNAPSHOT` | Request creation of a prepared snapshot |
| `CMD` | Default action after instance startup |

## 6.3 Syntax rules

- Directives are case-insensitive.
- Comments begin with `#`.
- Paths are relative to the build context.
- Quoted strings must be supported.
- Invalid directives produce actionable errors.
- Unknown Android versions must fail during resolution.
- File paths must be validated.
- Resource limits must be validated against host and backend capabilities.
- Duplicate singleton directives follow a documented rule; preferably reject ambiguous duplicates.
- Parser errors must include line and column information.

Example:

```text
Dorkerfile:8:1
error: unsupported directive "MEM"

Did you mean "MEMORY"?
```

## 6.4 Android image identification

Initially support:

```dockerfile
FROM android:35
```

Future syntax:

```dockerfile
FROM android:35-google_apis
FROM android:35-aosp
```

The implementation must discover supported images from actual SDK metadata.

Do not hardcode the assumption that every API level is available on every host architecture.

## 6.5 Build lockfile

A successful build should generate or update `Dorkerfile.lock`.

The lockfile records:

- Android API level.
- System-image package path and revision.
- Emulator version.
- Host architecture.
- Device configuration.
- APK content hashes.
- Resolved build options.

Example conceptual structure:

```toml
version = 1

[android]
api = 35
abi = "arm64-v8a"
system_image_revision = "resolved-at-build"

[emulator]
version = "resolved-at-build"

[application]
path = "./app-debug.apk"
sha256 = "resolved-at-build"
```

The exact format may evolve.

A successful build from an unchanged locked environment should resolve the same underlying artifacts, subject to their continued availability.

---

# 7. CLI Specification

The CLI binary must be named `dorker`.

Use Rust's `clap` for argument parsing.

## 7.1 `dorker doctor`

Check system requirements.

```bash
dorker doctor
```

Validate:

- macOS version and CPU architecture.
- Android SDK location.
- Emulator binary availability.
- ADB availability.
- Hardware acceleration support.
- Available disk space.
- Installed system images.
- Relevant permissions and runtime dependencies.

Display actionable remediation commands.

## 7.2 `dorker build`

Build a reusable Android image from a Dorkerfile.

```bash
dorker build .
dorker build -f Dorkerfile -t myapp:latest .
```

Required behaviour:

1. Parse and validate the Dorkerfile.
2. Resolve Android SDK dependencies.
3. Calculate the build configuration fingerprint.
4. Check the cache.
5. Create a build-time Android instance when needed.
6. Wait for verified Android readiness.
7. Install APKs.
8. Apply settings.
9. Save a snapshot when requested and supported.
10. Register the resulting logical image.
11. Clean up temporary processes.

Builds must be deterministic where supported by the backend.

The builder must never mutate a previously published image.

## 7.3 `dorker images`

```bash
dorker images
```

Example output:

```text
REPOSITORY    TAG       ANDROID   SIZE
myapp         latest    35        3.2GB
myapp         testing   35        3.1GB
```

Image size must distinguish logical referenced size from actual additional disk usage where appropriate.

## 7.4 `dorker run`

Launch an instance.

```bash
dorker run myapp:latest
dorker run -d --name test-1 myapp:latest
dorker run --rm myapp:latest
```

Required options:

- `--name`
- `--detach` / `-d`
- `--rm`
- `--memory`
- `--cpus`
- `--adb-port`

Required behaviour:

- Validate the image.
- Allocate a unique instance ID.
- Create independent writable state.
- Allocate an available emulator console/ADB port pair.
- Start or restore the emulator.
- Wait until Android is usable.
- Register the instance.
- Execute the default command, if defined.

`--rm` removes the instance after it exits or is stopped, without modifying the source image.

## 7.5 `dorker ps`

```bash
dorker ps
dorker ps -a
```

Example:

```text
ID        NAME       IMAGE           STATUS    ADB
a1b2c3    test-1     myapp:latest    RUNNING   5554
```

Use actual emulator serial or address formats when displaying ADB connections.

## 7.6 `dorker exec`

```bash
dorker exec test-1 "pm list packages"
dorker exec test-1 "input tap 400 600"
```

Execute Android shell commands through ADB.

Support stdout, stderr and exit status propagation.

Commands must target the correct instance even when other ADB devices are connected.

## 7.7 `dorker install`

```bash
dorker install test-1 ./app-debug.apk
```

Install an APK into an existing instance.

Errors must be surfaced directly.

## 7.8 `dorker screenshot`

```bash
dorker screenshot test-1 screenshot.png
```

Capture the Android virtual display.

This must work in headless mode where the backend supports capture.

## 7.9 `dorker logs`

```bash
dorker logs test-1
dorker logs test-1 --follow
```

Stream Android logcat for the specified instance.

## 7.10 `dorker stop` and `dorker rm`

```bash
dorker stop test-1
dorker rm test-1
dorker rm -f test-1
```

`stop` terminates the running emulator.

`rm` removes managed instance data.

Image data and shared cached layers must remain untouched.

## 7.11 `dorker test`

Provide a high-level instrumentation testing command.

```bash
dorker test test-1 \
  --runner com.example.test/androidx.test.runner.AndroidJUnitRunner
```

The CLI must:

- Verify the instance is ready.
- Start the specified instrumentation runner.
- Capture test output.
- Return a nonzero exit status on test failure.
- Preserve logs useful for diagnosing failures.

Automatic test APK discovery is deferred.

---

# 8. Rust Architecture

## 8.1 Workspace structure

Recommended layout:

```text
dorker/
├── Cargo.toml
├── crates/
│   ├── dorker-cli/
│   ├── dorker-core/
│   ├── dorker-config/
│   ├── dorker-runtime/
│   ├── dorker-adb/
│   ├── dorker-images/
│   ├── dorker-cache/
│   └── dorker-bench/
├── examples/
│   ├── basic/
│   └── instrumentation/
├── docs/
│   ├── architecture.md
│   ├── research.md
│   └── dorkerfile.md
└── tests/
```

Do not create crates merely to satisfy this layout. Consolidate modules if simpler.

## 8.2 Technology choices

| Component | Technology |
|---|---|
| Language | Rust, current stable |
| CLI | clap |
| Async execution | Tokio |
| Configuration | serde |
| Error handling | anyhow / thiserror |
| Logging | tracing |
| Persistent metadata | SQLite or atomic structured files |
| Emulator backend | Official Google Android Emulator |
| Device communication | ADB |
| Hashing | SHA-256 |
| macOS virtualisation | Existing emulator integration with Apple hypervisor |

Prefer invoking supported emulator and SDK interfaces rather than linking directly against QEMU internals.

## 8.3 Backend abstraction

Design the runtime to allow future backends.

```rust
#[async_trait]
pub trait RuntimeBackend: Send + Sync {
    async fn start(
        &self,
        config: &InstanceConfig,
    ) -> Result<RunningInstance>;

    async fn stop(
        &self,
        instance: &InstanceId,
    ) -> Result<()>;

    async fn status(
        &self,
        instance: &InstanceId,
    ) -> Result<InstanceStatus>;
}
```

Snapshot functionality should be capability-driven rather than assumed to be universally available.

Possible future backends:

- Official Android Emulator.
- Linux KVM-backed emulator.
- Remote Android execution service.

Only the official local emulator backend is required for v0.1.

## 8.4 Daemon strategy

Do **not** require a persistent daemon for the initial MVP.

Use a CLI with durable instance metadata and operating-system process inspection.

The CLI must be capable of discovering instances started by previous invocations.

A daemon may be introduced when required for:

- Warm instance pools.
- Multi-client coordination.
- Resource scheduling.
- Remote APIs.
- Background health monitoring.

Avoid premature architectural complexity.

---

# 9. Image and Storage Architecture

## 9.1 Logical image model

A dorker image consists of:

1. A resolved Android base system image.
2. A device configuration.
3. Application artifacts.
4. Provisioning instructions.
5. Optional prepared userdata or snapshot.
6. Metadata and content hashes.

This is not a conventional OCI container image in v0.1.

Avoid claiming OCI compatibility unless implemented and validated.

## 9.2 Storage layout

Suggested structure:

```text
~/.dorker/
├── images/
├── instances/
├── cache/
├── snapshots/
├── locks/
└── metadata/
```

Every active instance must have isolated writable data.

## 9.3 Build caching

Cache keys should consider:

- Android system-image identity.
- Emulator version.
- Architecture.
- Device configuration.
- APK hashes.
- Provisioning directives.
- Snapshot compatibility.

Changing only an APK should not trigger another Android SDK download.

However, creating a new prepared guest state may still require booting Android and installing the new APK.

## 9.4 Snapshot strategy

Investigate and use the Android Emulator's supported snapshot functionality.

Expected workflow:

1. Create clean instance.
2. Boot Android.
3. Verify boot completion.
4. Install applications.
5. Configure guest settings.
6. Save supported snapshot.
7. Stop instance.
8. Register cached image artifact.

On subsequent launches, restore the compatible snapshot with isolated instance storage.

Important requirements:

- Snapshots must not be shared concurrently as mutable state.
- Detect incompatible emulator versions.
- Detect incompatible system images.
- Detect incompatible VM configurations.
- Fall back to cold boot when necessary.
- Avoid modifying the source snapshot.

Actual snapshot portability and sharing semantics must be established experimentally.

## 9.5 Storage isolation

For v0.1, use safe per-instance copies or backend-supported overlays.

Copy-on-write optimisation may be implemented after evaluating filesystem and emulator compatibility.

Correctness and isolation take precedence over storage savings.

---

# 10. Instance Lifecycle

Every instance must have an explicit lifecycle.

```text
CREATED
   |
   v
STARTING
   |
   v
BOOTING
   |
   v
READY
   |
   v
STOPPING
   |
   v
STOPPED
   |
   v
REMOVED
```

Additional terminal/error states:

```text
FAILED
CRASHED
```

Requirements:

- No stale instance records after normal termination.
- Detect emulator crashes.
- Recover metadata after CLI interruption.
- Detect orphaned emulator processes.
- Prevent accidental instance-name collisions.
- Prevent simultaneous mutation of the same instance.
- Safely allocate and release ports.
- Validate PID identity before terminating a process.

Readiness must not rely solely on `adb wait-for-device`.

It should also confirm Android boot completion and required package-manager availability.

---

# 11. Performance Requirements

Performance is an explicit product objective, but improvements must be demonstrated rather than assumed.

## 11.1 Benchmark metrics

Measure:

| Metric | Description |
|---|---|
| Cold boot time | Process launch until verified Android readiness |
| Snapshot restore time | Restore initiation until verified readiness |
| Build time | Total Dorkerfile build duration |
| APK install time | Time required to install a representative APK |
| App launch time | Launch command until defined readiness signal |
| Idle CPU | Host CPU usage after stabilisation |
| Idle memory | Host physical memory attributable to emulator and manager |
| Disk usage | Physical storage occupied by image and instance |
| Cleanup time | Time required to stop and remove an instance |

## 11.2 Benchmark baselines

Compare against:

1. Official Android Emulator started headlessly without snapshots.
2. Official Android Emulator with snapshots enabled.
3. dorker using the same underlying emulator and compatible configuration.
4. dorker using prepared images and caching.

Where possible, use matching Android versions, CPU allocations, display configurations and guest services.

## 11.3 Proposed targets

These are aspirational targets, not guarantees.

| Metric | Goal |
|---|---|
| Prepared instance startup | Under 5 seconds |
| Cold boot | Under 20 seconds |
| Idle guest RAM | 2 GB configuration supported |
| Idle CPU | Minimal stable activity |
| Repeated identical build | Avoid repeating provisioning work |
| Instance cleanup | No orphan processes or temporary userdata |
| CLI command overhead | Negligible relative to emulator operations |

Do not claim a 2 GB RAM allocation means 2 GB or less of host memory consumption.

If targets cannot be met, publish measured results and explain the limiting factors.

## 11.4 Benchmark command

Provide:

```bash
dorker bench
```

Output should include machine details, runtime configuration and structured metrics.

Support JSON output for automated regression comparisons.

---

# 12. Headless Operation

Headless execution is a first-class requirement.

The runtime must support:

- Launch without opening an emulator window.
- ADB access.
- APK installation.
- Shell commands.
- Screenshot capture.
- Instrumentation tests.
- Logcat streaming.

Headless mode does not necessarily eliminate the Android display compositor or graphics workload.

The implementation must preserve enough rendering functionality for normal application behaviour.

GPU and software-rendering configurations should be benchmarked rather than selected through assumptions.

---

# 13. Developer Experience

## 13.1 First-run experience

The ideal first-run process:

```bash
brew install dorker
dorker doctor
dorker init
```

`dorker init` should generate a minimal Dorkerfile and explain how to build it.

Homebrew distribution is a release objective, not required for the first development milestone.

## 13.2 Error messages

Errors must describe:

- What failed.
- Why it likely failed.
- How to resolve it.
- Which instance or build operation was affected.

Example:

```text
error: Android API 35 ARM64 system image is missing

Run:
  sdkmanager "system-images;android-35;google_apis;arm64-v8a"

Or allow dorker to install the required image.
```

Package names must be resolved from actual SDK metadata.

## 13.3 JSON output

Operational commands should eventually support:

```bash
dorker ps --json
dorker images --json
dorker doctor --json
```

This is particularly important for AI agents and CI automation.

## 13.4 Exit codes

Define stable nonzero exit codes for:

- Invalid configuration.
- Missing dependency.
- Build failure.
- Instance startup failure.
- ADB operation failure.
- Test failure.
- Resource conflict.

---

# 14. Security and Reliability

## 14.1 Host safety

- Never execute arbitrary host commands from a Dorkerfile.
- Reject build-context path traversal.
- Treat APKs as untrusted executable artifacts.
- Avoid exposing ADB over public network interfaces by default.
- Use private local control interfaces.
- Sanitize process arguments and file paths.
- Do not use shell command interpolation for emulator invocations.

## 14.2 Resource safety

- Enforce configurable per-instance resource limits.
- Validate available host resources.
- Detect port collisions.
- Prevent accidental deletion of unrelated AVDs.
- Only manage instances created and registered by dorker.
- Clean up temporary files when builds fail.

## 14.3 Data isolation

- Never share mutable userdata between simultaneous instances.
- Ensure instance deletion does not modify cached images.
- Keep host credentials outside guest images.
- Warn users before exporting images containing sensitive application data.

## 14.4 Licensing

Respect Android SDK distribution terms.

Do not redistribute Google proprietary images or services without appropriate rights.

A future public image registry must distinguish redistributable artifacts from locally resolved SDK dependencies.

---

# 15. Testing Strategy

## 15.1 Unit tests

Cover:

- Dorkerfile parsing.
- Configuration validation.
- Hash generation.
- Path resolution.
- Instance state transitions.
- Metadata persistence.
- Cache invalidation.

## 15.2 Integration tests

Cover:

- Emulator process startup.
- ADB connection.
- APK installation.
- Screenshot capture.
- Guest shell execution.
- Graceful shutdown.
- Abnormal process termination.
- Duplicate instance names.
- Resource and port conflicts.

## 15.3 End-to-end tests

Use a small example Android application.

Required scenario:

```bash
dorker build -t example:test .
dorker run -d --name integration example:test
dorker exec integration "pm list packages"
dorker screenshot integration screenshot.png
dorker stop integration
dorker rm integration
```

Verify that:

- The app is installed.
- The emulator reaches READY.
- ADB commands succeed.
- A valid screenshot is generated.
- All managed resources are cleaned up.

## 15.4 Failure tests

Explicitly test:

- Missing Android SDK.
- Missing system image.
- Emulator crash during startup.
- Corrupted snapshot.
- Invalid APK.
- Unavailable ports.
- Disk exhaustion.
- Interrupted build.
- Concurrent instance creation.

---

# 16. Implementation Roadmap

## Phase 0 — Research and technical validation

Deliverables:

- `docs/research.md`
- `docs/architecture.md`
- Minimal proof of concept launching the Android Emulator headlessly on Apple Silicon.
- Verified ADB readiness.
- Snapshot capability experiments.
- Initial performance measurements.

**Exit criterion:** A headless ARM64 emulator can be started, controlled and stopped reliably from Rust.

## Phase 1 — Core CLI

Implement:

- Rust workspace.
- `dorker doctor`
- `dorker ps`
- `dorker run`
- `dorker stop`
- `dorker rm`
- `dorker exec`
- `dorker install`
- `dorker logs`
- `dorker screenshot`

Use existing AVDs if necessary during this phase.

**Exit criterion:** A developer can launch and control a disposable Android instance entirely through dorker.

## Phase 2 — Dorkerfile and image builder

Implement:

- Dorkerfile parser.
- Android system-image resolution.
- Build configuration.
- APK installation during provisioning.
- Logical image metadata.
- `dorker build`
- `dorker images`
- Build-context hashing.

**Exit criterion:** A developer can commit a Dorkerfile, build a configured Android image and launch it repeatedly.

## Phase 3 — Caching and snapshots

Implement:

- Compatible prepared snapshots.
- Build cache.
- Snapshot invalidation.
- Immutable image management.
- Isolated writable instance data.
- Measured startup optimisation.

**Exit criterion:** Repeated identical builds reuse cached artifacts, and restored instances are isolated and functionally equivalent to freshly provisioned instances.

## Phase 4 — Testing and automation

Implement:

- `dorker test`
- JSON output.
- Stable exit codes.
- `dorker bench`
- CI examples.
- Improved logging and diagnostics.

**Exit criterion:** CI and coding agents can build, launch, test and destroy Android environments without interactive intervention.

## Phase 5 — Distribution

Implement:

- Release binaries for macOS ARM64.
- Homebrew formula.
- Installation documentation.
- Sample Dorkerfiles.
- Comprehensive README.
- GitHub Actions build/test pipelines.

**Exit criterion:** A new user can install dorker and execute the documented basic workflow on a supported machine.

---

# 17. Future Capabilities

These must not delay the MVP.

### 17.1 Linux backend

Support Linux ARM64/x86_64, potentially integrating techniques or components from Google's Android Emulator Container Scripts.

### 17.2 Warm instance pools

Maintain prebooted instances to reduce allocation latency.

Warm pools must have explicit resource budgets and idle shutdown policies.

### 17.3 Docker Compose-style orchestration

Example future `dorker-compose.yml`:

```yaml
services:
  phone:
    build: .
    memory: 2GB

  tablet:
    image: android:35
    memory: 3GB
```

### 17.4 Remote execution

Provide a daemon and authenticated API for remotely hosted Android instances.

### 17.5 AI-agent integrations

Expose capabilities through:

- Structured CLI JSON.
- MCP server.
- Local HTTP API.
- Screenshot and interaction utilities.

### 17.6 Image registry

Support sharing image definitions and redistributable artifacts without violating Android SDK or Google service licensing.

### 17.7 Android Automotive

Investigate dedicated Android Automotive OS guest environments and head-unit test orchestration.

Do not treat Android Auto and Android Automotive OS as interchangeable.

### 17.8 Live display

Optional browser-based or native viewer using supported emulator screenshot, streaming or gRPC interfaces.

---

# 18. Explicit Non-Goals

The initial implementation must NOT:

- Implement a new CPU emulator.
- Implement Android itself.
- Implement a custom Android kernel.
- Rewrite QEMU in Rust.
- Require Docker Desktop.
- Build a graphical desktop application.
- Create a hosted cloud service.
- Implement a public image registry.
- Implement full OCI compatibility.
- Support every emulator/device profile.
- Optimise for Windows.
- Promise lower RAM or CPU consumption without measurements.

The MVP is focused on **orchestration, reproducibility, developer experience and measurable startup optimisation**.

---

# 19. Definition of Done — v0.1

The release is complete when all the following conditions are met:

- [ ] Runs on supported macOS Apple Silicon machines.
- [ ] Uses the existing official Android Emulator.
- [ ] Requires no Docker Desktop installation.
- [ ] Reads and validates a Dorkerfile.
- [ ] Resolves supported Android system images.
- [ ] Builds a reusable logical image.
- [ ] Installs an APK during provisioning.
- [ ] Launches a headless Android instance.
- [ ] Supports multiple independently managed instances.
- [ ] Supports ADB shell execution.
- [ ] Captures screenshots.
- [ ] Streams logcat.
- [ ] Stops and removes instances reliably.
- [ ] Does not modify unrelated AVDs.
- [ ] Cleans up failed or interrupted operations.
- [ ] Provides useful errors and diagnostics.
- [ ] Includes unit and end-to-end tests.
- [ ] Includes baseline performance measurements.
- [ ] Documents current limitations.
- [ ] Includes clear setup and usage instructions.

Snapshot support is a primary performance objective but may be marked experimental if backend limitations prevent reliable isolation or restoration.

---

# 20. Instructions for the Coding Agent

You are implementing **dorker**, a Docker-inspired Android environment manager written in Rust.

Follow these instructions:

1. **Research before coding.** Investigate Google's Android Emulator Container Scripts and official Android Emulator capabilities. Document your findings.
2. **Reuse existing technology.** Do not implement new virtualisation infrastructure.
3. **Prioritise macOS Apple Silicon.** Make native ARM64 operation work before considering other platforms.
4. **Implement incrementally.** Build a working headless runtime before implementing the Dorkerfile builder.
5. **Avoid unnecessary abstractions.** Use the simplest architecture that supports the stated requirements.
6. **Prefer official interfaces.** Use supported emulator, SDK and ADB commands.
7. **Write automated tests.** Particularly for parsing, state management, lifecycle recovery and caching.
8. **Measure performance.** Do not assume Rust orchestration makes the Android Emulator faster.
9. **Maintain documentation.** Record architecture decisions, limitations and performance results.
10. **Keep the CLI coherent.** Commands must be consistent, discoverable and scriptable.

### First implementation task

Produce `docs/research.md` and `docs/architecture.md`, then implement a minimal Rust CLI capable of:

```bash
dorker doctor
dorker run --name test android:35
dorker ps
dorker exec test "getprop ro.build.version.sdk"
dorker screenshot test output.png
dorker stop test
dorker rm test
```

For this initial milestone, `android:35` may resolve to a locally installed compatible system image. Automatic downloading and image building can follow later.

Do not proceed with a custom VMM or elaborate daemon architecture.

**The initial goal is a reliable, headless, native macOS Android execution environment managed entirely through Rust.**

---

## 21. Long-Term Product Vision

A developer should eventually be able to clone any Android project containing a Dorkerfile and run:

```bash
dorker build .
dorker run --rm myapp:latest
```

The developer should not need to understand AVD internals, manually provision Android devices, or coordinate emulator processes.

The same workflow should eventually work for local development, automated testing, CI infrastructure and autonomous coding agents.

**dorker's purpose is not to reinvent Android emulation. It is to make Android environments reproducible, disposable and effortless to operate.**
