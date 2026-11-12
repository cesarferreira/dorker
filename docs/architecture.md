# Architecture

dorker orchestrates the official Android Emulator. It does not emulate anything
itself. One crate, synchronous, no daemon.

## Modules

| Module | Responsibility |
|---|---|
| `main.rs` | clap CLI surface and dispatch |
| `commands.rs` | `run`, `start`, `pull`, `ps`, `exec`, `install`, `screenshot`, `logs`, `stop` (`--all`, `--orphans`), `rm`, `images`, `rmi`; the pull-on-demand policy |
| `commit.rs` | `dorker commit`: save a running instance as an image |
| `config.rs` | the optional config file (`home`, `sdk`, `gpu`, `min_free_mb`) and `dorker config` |
| `templates.rs` | `dorker templates` / `dorker init --template`; the folders under `templates/` are embedded by the root `build.rs` |
| `doctor.rs` | host checks with remediation (`--json`) |
| `sdk.rs` | locate the SDK, list installed and published system images, resolve `android:<api>[-flavour]` |
| `versions.rs` | `dorker versions` |
| `store.rs` | instance metadata, lifecycle states, create lock, ids, the store location |
| `runtime.rs` | AVD materialisation, port allocation, emulator spawn, readiness, signalling, ADB |
| `dorkerfile.rs` | pure Dorkerfile parser with line/column errors |
| `build.rs` | `dorker build`: `FETCH`, fingerprinting, provisioning, `PROVISION`, atomic publish, lockfile |
| `xapk.rs` | install an `.apk` or an `.xapk` bundle (split APKs, OBB files) |
| `images.rs` | image metadata, tags, size accounting (clone-aware on APFS) |
| `ls.rs`, `prune.rs` | `dorker ls` (images, instances, storage) and `dorker prune` |
| `testcmd.rs` | `dorker test`: runs `am instrument -r`, parses the status protocol, keeps logs (`--out` copies them) |
| `testrun.rs` | `dorker test --image`: one shard per pool worker, the aggregate verdict, `summary.json` |
| `pool.rs` | worker pools: run ownership, resource admission, bounded parallel startup, cancellation and cleanup of a group of disposable instances |
| `bench.rs` | `dorker bench` |
| `errors.rs` | stable exit codes |
| `mcp.rs` | `dorker mcp`: MCP server on stdio; each tool runs the CLI with `--json` |
| `style.rs`, `log.rs` | colour rules; `-v/--verbose` stderr logging |
| `units.rs` | size and image-reference parsing |

## Decisions

1. **No daemon.** State lives in `~/.dorker` (`$DORKER_HOME`, or `home` in the config
   file; see decision 14). Each invocation
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
   `images/<fingerprint[..12]>`. `run` clones it (`cp -c` on macOS, `cp -R --reflink=auto`
   on Linux) and restores the snapshot from the clone. The clone is copy-on-write on APFS,
   on XFS created with `reflink=1`, and on Btrfs; on ext4 and NFS it is a full copy of the
   image. Images are never run directly: a failed restore deletes the snapshot it could
   not load.
4. **Fingerprint = cache key.** SHA-256 over system image + revision, emulator
   version, ABI, resources, device, resolution, headless, GPU mode, `INSTALL` file
   *content* hashes, `PROVISION` script content hashes, settings and snapshot name.
   Identical inputs reuse the image instantly. Paths are not part of it, so moving a
   project does not rebuild. `FETCH` scripts are not either: they only make the
   `INSTALL` files that are. `Dorkerfile.lock` records the resolved artifacts and
   warns on drift. Because the GPU mode and the ABI differ per machine, an image is
   per machine too.
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
   line contains `-avd dorker -port <port>` for this instance; the signal goes to its
    process group.
11. **Crash and orphan handling.** A process-expecting record whose process is gone
    becomes `CRASHED`; a `CREATED` record older than 60 s becomes `FAILED`; emulator
    processes with no record are reported by `dorker ps`, and `dorker stop --orphans`
    stops them after asking (they may belong to another store). Mutating commands (`stop`, `rm`,
    `start`) take a per-instance lock (`op.lock`, holder pid inside, stale locks from dead
    processes are broken); `stop` runs `sync` in the guest first so restarts keep data.
12. **Atomic metadata.** `instance.json` is written to a temp file and renamed.
   A lock file serialises name and port allocation across concurrent runs.
13. **Host scripts run automatically in the CLI.** `PROVISION` and `FETCH` run
    arbitrary code on the host while building, without a confirmation prompt or
    opt-in flag, including in CI. Only build trusted Dorkerfiles and scripts.
    The MCP server sets `DORKER_MCP=1` on the CLI it starts, which refuses to run
    host scripts. A cache hit runs nothing and remains usable through MCP.
    Installing missing system images still requires `--pull` or `DORKER_PULL=1`
    without a terminal; a global config file cannot enable it for an agent.
14. **Settings that differ per machine live in a config file** (`config.rs`):
    `$DORKER_CONFIG`, else `$XDG_CONFIG_HOME/dorker/config.toml`, else
    `~/.config/dorker/config.toml`. Precedence is flag, then environment variable, then
    the file, then the built-in default. Unit tests never read it, and the integration
    tests point `DORKER_CONFIG` at a private path.
15. **Templates are data.** `templates/<name>/` folders are embedded at build time, so
    `cargo install` ships them and a contribution is a folder. A test validates every
    template (metadata, the Dorkerfile parses, scripts are executable).
16. **Worker pools own what they start** (`pool.rs`). A pool resolves its image tag once to
    an image id, writes `runs/<run-id>/manifest.json` (owner pid, worker count, per-worker
    estimate), and starts every worker through `commands::start_in` with
    `owner = {run_id, worker}`. The owner is saved in `instance.json` before the emulator
    spawns, so partial starts are recoverable. Cleanup removes only instances with this run
    as owner, under each instance's `op.lock`, after re-reading it and checking process
    identity; it never uses `stop --all` or orphan matching. A run that died without
    cleaning up (SIGKILL) is detected by the next pool (owner pid gone, or no longer a
    dorker process) and its instances are removed then. Ctrl-C, SIGTERM and SIGHUP all set
    the same interrupt flag, which the pool's supervisor turns into cancellation.
17. **Admission is an estimate.** Before starting, a pool compares what its workers need
    (host memory estimated as 1.5x guest RAM + 512 MB, the clone's disk, a port pair, vCPUs)
    with the host's available memory and free disk minus a host reserve (2 GB,
    `DORKER_HOST_RESERVE_MB`) and minus what other live pools have claimed but not yet used.
    Running instances are not subtracted again (the host figures already include them). It
    refuses with exit code 8 rather than silently starting fewer workers, rechecks before
    each worker starts, and warns (but still admits) when a metric cannot be read. Claims are
    read and written under `locks/pool.lock`. It cannot guarantee the host never runs out of
    memory. Clones still happen under `create.lock`, so on filesystems without reflinks the
    workers' copy phase is serial.
18. **Sharded tests are strict** (`testrun.rs`). `dorker test --image` runs shard `i` of `N`
    on pool worker `i`, with the same runner and `--class` for every shard, and relies on
    AndroidJUnitRunner's `numShards`/`shardIndex` to split the suite. Workers are not
    reused, so there is no dynamic work queue. A shard counts only if its output reached
    the runner's final `INSTRUMENTATION_CODE: -1`; a missing final code, a crash, a
    timeout or a runner that did not start make the run incomplete, and a test reported by
    two shards (the runner ignored the shard arguments) fails it. Exit-code precedence:
    interrupt (130), startup failure (5), adb failure (6), incomplete or duplicated or
    failed or empty (7), cleanup failure (1), pass (0). `summary.json` is written (atomically)
    three times: with verdict `running` before any worker starts, with cleanup `pending`
    once every shard has ended, and after cleanup; `junit.xml` with the last two.
19. **Reports come from evidence.** Test durations are the host-side time between a test's
    start and end status lines as they stream in; tests without both lines have none. JUnit
    has one test case per reported result and never a synthesized pass; run-level problems
    are `<error>` cases. Before a worker is removed, its `emulator.log` is copied and, unless
    the shard passed or was cancelled, a screenshot is taken. Each collection step is bounded
    (`DORKER_COLLECT_TIMEOUT_SECS`, default 30) and its failure is recorded, never fatal.
20. **Store locks name their holder.** `create.lock` and `pool.lock` contain the holder's pid
    and are broken only when that process is gone (or the pid is no longer dorker's), not
    by age: without reflinks a clone can hold `create.lock` for minutes. Pool workers wait
    for it up to their startup timeout; other commands give up after 10 s.

## Build pipeline

`dorker build` (and `dorker run` with no image) does, in order:

1. Parse the Dorkerfile; if an `INSTALL` file is missing and a `FETCH` script exists,
   run it automatically in the CLI (refused through MCP).
2. Resolve the system image (offering to install it if missing) and hash the inputs into
   the fingerprint. An image with that id is a cache hit: stop here.
3. Otherwise check that MCP is not being asked to run `PROVISION` scripts, then boot
   a transient build instance that doubles as the workspace.
4. Install the `INSTALL` files, apply the `SETTING`s, then flush, reboot and verify they
   persisted (so a cold boot of the image has them).
5. Run the `PROVISION` scripts against the build emulator, then `sync`. They run after the
   reboot on purpose: what they start in memory stays running in the snapshot.
6. Save the snapshot, stop the emulator and publish the AVD home atomically.

The build emulator's steps (boot, each `INSTALL`, settings, persist, each
`PROVISION`, snapshot) are numbered `[n/N]` in the output.

## Lifecycle

```
CREATED -> STARTING -> BOOTING -> READY -> STOPPING -> STOPPED
              \           \         \________________/
               +-> FAILED  +-> FAILED     any live state -> CRASHED
STOPPED | FAILED | CRASHED -> STARTING (`dorker start`: cold boot, data kept)
```

Transitions are validated by `State::can_become`.

## On-disk layout

```
~/.dorker/
├── images/<id>/image.json        metadata; tags are the only mutable field
├── images/<id>/avd/              golden AVD home (userdata + snapshot)
├── images/<id>/build.log
├── instances/<id>/instance.json
├── instances/<id>/avd/           an image clone, or a fresh AVD (+ userdata)
├── instances/<id>/emulator.log
├── instances/<id>/test-*/        `dorker test` logs (die with the instance)
├── images/.<id>.partial/         a build or commit in progress (prune clears)
├── runs/<run-id>/manifest.json   a worker pool's owner and resource claim
├── results/<run-id>/             `dorker test --image` reports (without --out)
├── locks/create.lock             holds the holder's pid
└── locks/pool.lock
```

The store root is `~/.dorker` unless `DORKER_HOME` or the config file's `home` says
otherwise. A new instance is a clone of its image, which only works within one filesystem
and only where that filesystem can clone: APFS, XFS with reflinks, or Btrfs. Put the store
on a local disk of that kind. On NFS or ext4 every instance is a full copy.

## Not implemented yet

- Automatic test-APK discovery.
- Moving an image to another machine: a snapshot is host-specific, so there is no
  `export` / `import`. The recipe (Dorkerfile, `FETCH`, `PROVISION`) is the portable part.
- Validation on Linux arm64 and bare-metal Linux; Windows is unsupported.
- MCP progress notifications and cancellation (a build returns only when done).
- A `RuntimeBackend` trait (there is one backend).

Next: [Limitations →](reference/limitations.md)
