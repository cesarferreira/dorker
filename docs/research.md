# Research notes

Status: Phase 0, first milestone. Items marked **unverified** were not confirmed
and must be checked before being relied on.

## Google `android-emulator-container-scripts`

Source: <https://github.com/google/android-emulator-container-scripts> (README,
`REGISTRY.MD`, `LICENSE`; the Python sources were **not** read).

- **What it is:** Python 3.10+ scripts (`emu-docker`) that build Docker images
  containing the Android Emulator, plus a WebRTC gateway and React client.
  Marked experimental.
- **License:** Apache 2.0. Its registry notes say that using the images means
  accepting the Android SDK License Agreement.
- **Platform:** Linux hosts with KVM only. The README and `REGISTRY.MD` state
  that Docker Desktop on macOS and Windows is not supported. The examples are
  x86_64; Apple Silicon / ARM64 is not mentioned.
- **Discovery:** `emu-docker list` prints `dl.google.com/android/repository`
  URLs for system images and emulator builds (channels `stable`, `dev`). The
  exact manifest endpoint is **unverified** (likely in `emu/android_release_zip.py`).
- **ADB / gRPC / WebRTC:** ADB on container port 5555 (key via `ADBKEY`), emulator
  gRPC on 8554, and a gateway that bridges WebRTC signalling to gRPC.
- **Snapshots / userdata overlays:** none documented. **Unverified** in code.
- **Reusable by dorker:** the idea of resolving images from Google's repository
  manifests; the gRPC/WebRTC gateway as a possible future "live display"; the
  image-registry layout as a Linux backend. None of it runs on macOS.
- **What dorker adds:** native macOS ARM64 execution (no Docker, no KVM), a
  declarative `Dorkerfile`, per-instance isolation, instance lifecycle, and a
  Docker-style CLI.

## Confirmed on this machine (macOS 26.7, Apple Silicon, emulator 37.2.12)

| Fact | Evidence |
|---|---|
| The official emulator runs headless and natively on ARM64 | `qemu-system-aarch64-headless` under `Hypervisor.framework`; `sysctl kern.hv_support` = 1 |
| A private `ANDROID_AVD_HOME` works: no `avdmanager` or Java needed | dorker writes `config.ini` + `.ini` itself; existing AVDs in `~/.android/avd` are untouched (verified by listing before and after) |
| Several instances run concurrently with distinct console/ADB ports | two simultaneous `dorker run`, ports 5554 and 5556 |
| Commands must target by serial | a physical phone was attached the whole time; every call uses `adb -s emulator-<port>` |
| Cold boot to verified readiness is about 19-20 s | 19.7 s, 19.2 s, `-gpu swiftshader_indirect`, 2 GB, 2 CPUs, API 35 `google_apis` |
| Graceful stop takes under 1 s and leaves no processes | `adb emu kill`; checked with `ps` |
| The emulator launcher `exec`s into `qemu-system-aarch64-headless` | the recorded pid is the QEMU process, so signalling its process group is safe |

Single runs only; these are not benchmarks. See "Open questions".

## Emulator interfaces used

- `emulator -avd <name> -port <even> -no-window -no-audio -no-boot-anim -no-metrics -no-snapshot -gpu swiftshader_indirect`
- Readiness: `sys.boot_completed == 1` **and** `pm path android` succeeds
  (not just `adb wait-for-device`).
- Stop: `adb emu kill`, then `SIGTERM`, then `SIGKILL` to the process group,
  only after verifying the pid's command line names the instance.

## Snapshot and disk-overlay feasibility

Not yet experimented. The emulator advertises `-snapshot`, `-no-snapshot-save`,
`-no-snapshot-load`, `-force-snapshot-load`, `-snapshot-list`, `-snapstorage`
and `-check-snapshot-loadable <name>` (from `emulator -help`). The last one is
the likely compatibility check for Phase 3. Whether a snapshot can be copied to
a new instance with a different AVD id, port or userdata is **unverified** and
decides the Phase 3 design. v0.1 uses `-no-snapshot` (always cold boot).

## Licensing

- Google's scripts: Apache 2.0.
- The Android Emulator and system images fall under the Android SDK License
  Agreement. dorker uses the user's own installed SDK and never redistributes
  images. A future registry must keep redistributable artifacts separate from
  locally resolved SDK dependencies.

## Technical risks

1. **Snapshot portability** (see above), the main performance lever.
2. **Hand-written AVD `config.ini`**: not a stable public interface. Keys were
   copied from a working AVD and the instance boots, but emulator updates may
   change required keys. Mitigation: boot test in CI.
3. **Pid identity** relies on `ps -o command=` containing `dorker-<id>`.
4. **Host memory**: a 2 GB guest uses more host RAM than 2 GB. Not measured.
5. **Port range** 5554-5680 follows the emulator's console convention.
6. **Image resolution** only reads installed images; downloading via
   `sdkmanager` is deferred.

## Open questions

- Host RAM and idle CPU per instance (needs `dorker bench`).
- `-gpu host` vs `swiftshader_indirect` boot time and screenshot correctness.
- Whether `-read-only` AVD sharing could replace per-instance copies.
