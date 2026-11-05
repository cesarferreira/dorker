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
| `units.rs` | size and image-reference parsing |

## Decisions

1. **No daemon.** State lives in `~/.dorker` (`$DORKER_HOME`). Each invocation
   reconciles persisted state with the OS process table (`runtime::reconcile`),
   so instances survive CLI exit and crashes are detected lazily: an instance
   that should have a process but does not becomes `CRASHED`.
2. **Private AVD per instance.** dorker writes `config.ini` into
   `instances/<id>/avd` and sets `ANDROID_AVD_HOME` for that emulator. It never
   reads or writes `~/.android/avd`, so it cannot touch unrelated AVDs, and each
   instance has its own userdata (no shared mutable state).
3. **Synchronous Rust, no Tokio.** The PRD lists Tokio, but every operation is
   spawn-and-poll. Async can be added with the `RuntimeBackend` trait later.
4. **No `RuntimeBackend` trait yet.** There is one backend. Extracting the trait
   from `runtime.rs` is mechanical once a second backend exists.
5. **Never a shell.** Emulator and adb are invoked with argument vectors. The
   user's command goes to `adb shell` as a single argument and is interpreted by
   the guest, not the host.
6. **Always target by serial** (`adb -s emulator-<port>`); a physical device can
   be attached at the same time.
7. **Readiness** is `sys.boot_completed=1` and a working package manager.
8. **Pid identity before signals.** A process is signalled only if its command
   line contains `dorker-<id>`; the signal goes to its process group.
9. **Atomic metadata.** `instance.json` is written to a temp file and renamed.
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
├── instances/<id>/instance.json
├── instances/<id>/avd/           private ANDROID_AVD_HOME (+ userdata)
├── instances/<id>/emulator.log
└── locks/create.lock
```

`images/`, `cache/` and `snapshots/` arrive with `dorker build` (Phase 2-3).

## Not implemented yet

`dorker build`/`images`, Dorkerfile and lockfile, snapshots, `dorker test`,
`dorker bench`, `dorker init`, stable exit codes beyond 0/1 (doctor failure is 2),
automatic system-image download, restart of stopped instances.
