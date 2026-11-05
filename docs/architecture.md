# Architecture

dorker orchestrates the official Android Emulator. It does not emulate anything
itself. One crate, synchronous, no daemon.

## Modules

| Module | Responsibility |
|---|---|
| `main.rs` | clap CLI surface and dispatch |
| `commands.rs` | `run`, `ps`, `exec`, `install`, `screenshot`, `logs`, `stop`, `rm` |
| `doctor.rs` | host checks with remediation (`--json`) |
| `sdk.rs` | locate the SDK, list installed system images, resolve `android:<api>[-flavour]` |
| `store.rs` | instance metadata, lifecycle states, create lock, ids |
| `runtime.rs` | AVD materialisation, port allocation, emulator spawn, readiness, signalling, ADB |
| `dorkerfile.rs` | pure Dorkerfile parser with line/column errors |
| `build.rs` | `dorker build`: fingerprinting, provisioning, atomic publish, lockfile |
| `images.rs` | image metadata, tags, size accounting |
| `units.rs` | size and image-reference parsing |

## Decisions

1. **No daemon.** State lives in `~/.dorker` (`$DORKER_HOME`). Each invocation
   reconciles persisted state with the OS process table (`runtime::reconcile`),
   so instances survive CLI exit and crashes are detected lazily: an instance
   that should have a process but does not becomes `CRASHED`.
2. **Private AVD per instance.** Each instance has its own `ANDROID_AVD_HOME`
   (`instances/<id>/avd`) containing one AVD named `dorker`. dorker never reads or
   writes `~/.android/avd`, so it cannot touch unrelated AVDs, and each instance
   has its own userdata (no shared mutable state). The constant name is required:
   the emulator will not restore a snapshot into an AVD with a different identity.
   Process identity is therefore `-avd dorker -port <port>` plus the recorded pid.
3. **Images are golden AVD homes.** `build` boots an AVD, provisions it, saves a
   snapshot, and publishes the directory atomically (`.partial` rename) to
   `images/<fingerprint[..12]>`. `run` clones it (`cp -c`, copy-on-write on APFS)
   and restores the snapshot from the clone. Images are never run directly: a
   failed restore deletes the snapshot it could not load.
4. **Fingerprint = cache key.** SHA-256 over system image + revision, emulator
   version, ABI, resources, device, resolution, headless, APK *content* hashes,
   settings and snapshot name. Identical inputs reuse the image instantly.
   `Dorkerfile.lock` records the resolved artifacts and warns on drift.
5. **Synchronous Rust, no Tokio.** The PRD lists Tokio, but every operation is
   spawn-and-poll. Async can be added with the `RuntimeBackend` trait later.
6. **No `RuntimeBackend` trait yet.** There is one backend. Extracting the trait
   from `runtime.rs` is mechanical once a second backend exists.
7. **Never a shell.** Emulator and adb are invoked with argument vectors. The
   user's command goes to `adb shell` as a single argument and is interpreted by
   the guest, not the host.
8. **Always target by serial** (`adb -s emulator-<port>`); a physical device can
   be attached at the same time.
9. **Readiness** is `sys.boot_completed=1` and a working package manager.
10. **Pid identity before signals.** A process is signalled only if its command
   line contains `dorker-<id>`; the signal goes to its process group.
11. **Atomic metadata.** `instance.json` is written to a temp file and renamed.
   A lock file serialises name and port allocation across concurrent runs.

## Lifecycle

```
CREATED -> STARTING -> BOOTING -> READY -> STOPPING -> STOPPED
              \           \         \________________/
               +-> FAILED  +-> FAILED     any live state -> CRASHED
STOPPED | FAILED | CRASHED -> STARTING (restart; not exposed by the CLI yet)
```

Transitions are validated by `State::can_become`.

## On-disk layout

```
~/.dorker/
├── images/<id>/image.json        metadata; tags are the only mutable field
├── images/<id>/avd/              golden AVD home (userdata + snapshot)
├── images/<id>/build.log
├── instances/<id>/instance.json
├── instances/<id>/avd/           clone of an image, or a fresh AVD (+ userdata)
├── instances/<id>/emulator.log
└── locks/create.lock
```

## Not implemented yet

`dorker test`, `dorker bench`, `dorker init`, stable exit codes beyond 0/1 (doctor
failure is 2), automatic system-image download, restart of stopped instances,
copy-on-write verification beyond APFS `cp -c`.
