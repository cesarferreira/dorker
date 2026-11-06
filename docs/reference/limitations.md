# Limitations

- **Platform.** Validated on macOS Apple Silicon (primary) and Linux x86_64 with KVM (Ubuntu 24.04, an Azure VM with nested virtualisation; unit tests and the full emulator end-to-end suite pass). Linux arm64 and bare-metal Linux are untested. Windows is unsupported. Linux is slower in our measurements (cold boot about 29s, snapshot restore about 9s; see [Benchmarks](../benchmarks.md)).
- **System images are installed on request, not automatically.** `dorker pull android:35` runs Google's `android sdk install` (the replacement for the deprecated `sdkmanager`; older command-line tools fall back to `sdkmanager`). The new CLI accepts the Android SDK licences (and the ARM translation licence for arm64 images) for you without prompting, so `pull` says so first, and Google documents that it collects usage data (opt out with `android --no-metrics`). `dorker run` and `dorker build` print this command when an image is missing. The download path itself was only verified for the already-installed and nonexistent-package cases, not a real multi-GB download.
- **Snapshots are strict.** A change of emulator version, system-image revision, memory, or CPUs invalidates an image's snapshot. dorker then cold boots (about 15s) and says so. Snapshots are host-specific and not portable between machines.
- **Memory.** A 2GB guest used about 2.5GB of host memory after a cold boot and 3.3GB after a snapshot restore on macOS (3–3.7GB on Linux). Idle CPU is dominated by Android background work and takes about a minute to settle.
- **Builds take about 45s** because dorker reboots the guest to persist provisioned state, plus the time of any `INSTALL`, `FETCH`, and `PROVISION` steps.
- **`CMD`** only supports `launch <package>`. `dorker start` always cold boots (about 14s) because restoring the image snapshot would roll back the instance's data. Rendering uses the Mac GPU (`host`) on macOS and software rendering (`swiftshader_indirect`) elsewhere. `GPU <mode>` in the Dorkerfile selects another. No mode was faster for snapshot restore in our measurements (see [Benchmarks](../benchmarks.md)). There is no automatic test-APK discovery.
- **Disk clones.** An instance is a copy-on-write clone of its image on APFS, and on Linux XFS (the filesystem must be created with `reflink=1`) and Btrfs. ext4 and NFS have no clone operation, so each instance is a full copy of the image. `dorker ls` reports a clone's private size only on macOS. On Linux it counts shared blocks again.
- **Concurrency.** Creating instances concurrently is safe. `stop`, `rm`, and `start` on the same instance are serialised by a per-instance lock (the second command waits up to 60s, then exits 8). Pool workers wait for the shared creation lock for up to their startup timeout, because a full copy (no reflink) can hold it for minutes.
- **Orphans.** Emulator processes that dorker started but lost track of (the CLI was killed mid-start) are reported by `dorker ps`. `dorker stop --orphans` stops them after asking. dorker never adopts them.
- **Examples.** `examples/basic` and `examples/instrumentation` use tiny hand-built APKs. `examples/counter-app` is the realistic one.

The design notes and the list of things not implemented yet are in [Architecture](../architecture.md).

## Exit codes

| Code | Meaning |
|---|---|
| 0 | success |
| 1 | other error |
| 2 | invalid configuration or arguments (including Dorkerfile errors) |
| 3 | missing dependency (SDK, emulator, adb, system image); `doctor` failures |
| 4 | image build failed |
| 5 | instance failed to start or boot |
| 6 | adb operation failed |
| 7 | instrumentation tests failed, or a sharded run was incomplete |
| 8 | name, port, or build conflict, not enough free disk space, or not enough capacity for `--workers` |
| 130 | `dorker test --image` was interrupted (its workers are removed) |

`dorker exec` returns the guest command's own exit status.

Next: [Development →](development.md)
