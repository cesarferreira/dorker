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

- `emulator -avd dorker -port <even> -no-window -no-audio -no-boot-anim -no-metrics -gpu swiftshader_indirect`
  plus `-no-snapshot` (base runs), `-no-snapshot-load -no-snapshot-save` (builds, so
  the snapshot is saved explicitly) or `-snapshot <name> -no-snapshot-save` (image runs)
- Readiness: `sys.boot_completed == 1` **and** `pm path android` succeeds
  (not just `adb wait-for-device`).
- Stop: `adb emu kill`, then `SIGTERM`, then `SIGKILL` to the process group,
  only after verifying the pid's command line names the instance.

## Snapshot and disk-overlay feasibility (experimentally established)

Setup: seed AVD booted cold, state changed, `adb emu avd snapshot save ready`,
emulator killed; the AVD directory was then copied and booted elsewhere.

| Experiment | Result |
|---|---|
| Restore into a copy with the **same AVD name**, different directory and different console port | Works. `Successfully loaded snapshot 'ready' using 1186 ms`; verified-ready in 4.0 s vs 14-19 s cold. RAM state and userdata survive |
| Restore into a copy whose AVD was **renamed** (`AvdId`, `.ini`, directory) | Fails: `Failed to load snapshot, because 'different AVD configuration'` |
| A failed restore | The emulator **deletes the snapshot** it could not load (and exits with `-force-snapshot-load`) |
| `fastboot.forceColdBoot = yes` in `config.ini` | Silently turns every boot into `-no-snapshot`. The emulator rewrites `config.ini` with spaces around `=`, so edits must tolerate both forms |
| Snapshot size | About 2 GB for a 2 GB guest (`ram.bin`); an image is about 3.3 GB logical |
| Copying an image to an instance | `cp -c -R` (APFS clone) is used, falling back to a plain copy |

Consequences for the design:

1. Every dorker instance uses the AVD name `dorker`. Isolation comes from a private
   `ANDROID_AVD_HOME` per instance, not from unique AVD names.
2. Instances restore from **their own clone** of the image, never from the image,
   because a failed restore deletes the snapshot.
3. Anything the emulator compares must match the image: memory, CPUs, display,
   system-image revision and emulator version. dorker checks these before
   restoring and cold boots with a note when they differ, so `--memory`/`--cpus`
   overrides disable the snapshot.
4. Instances start with `-snapshot <name> -no-snapshot-save`, so the clone is never
   mutated by the snapshot machinery.

Measured with `dorker run` on this machine (API 35 `google_apis`, 2 GB, 2 CPUs,
`swiftshader_indirect`): snapshot restore 2.3-3.8 s to verified readiness across
5 runs; cold boot 14.4-19.7 s across 6 runs. These are ad hoc runs, not a
benchmark.

## Licensing

- Google's scripts: Apache 2.0.
- The Android Emulator and system images fall under the Android SDK License
  Agreement. dorker uses the user's own installed SDK and never redistributes
  images. A future registry must keep redistributable artifacts separate from
  locally resolved SDK dependencies.

## Technical risks

1. **Snapshot compatibility** is strict (see above): any emulator or system-image
   update invalidates existing images' snapshots. dorker falls back to cold boot.
2. **Hand-written AVD `config.ini`**: not a stable public interface. Keys were
   copied from a working AVD and the instance boots, but emulator updates may
   change required keys. Mitigation: boot test in CI.
3. **Pid identity** relies on `ps -o command=` containing `-avd dorker -port <port>`.
4. **Host memory**: a 2 GB guest used about 5.5 GB of host RSS (see `benchmarks.md`).
5. **Port range** 5554-5680 follows the emulator's console convention.
6. **Image resolution** only reads installed images; downloading via
   `sdkmanager` is deferred.

## Open questions

- Host RAM and idle CPU per instance (needs `dorker bench`).
- `-gpu host` vs `swiftshader_indirect` boot time and screenshot correctness.
- Whether `-read-only` AVD sharing could replace per-instance copies.
- Whether a snapshot is portable across machines (almost certainly not: it
  embeds host-specific state), which limits any future image registry.
