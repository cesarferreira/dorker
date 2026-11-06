# Research notes

Status: Phase 0 research log, kept as a record. Later work answered several of the open
questions at the end; see `benchmarks.md` and `architecture.md` for the current state.
Original status: Phase 0, first milestone. Items marked **unverified** were not confirmed
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

Provisioned state and cold boots (found by testing, not documented anywhere I read):

| Experiment | Result |
|---|---|
| Provision, save snapshot, `emu kill`, then **cold boot** the image | The APK and `settings put` values are **missing**: they only existed in the snapshot's RAM state |
| `settings put`, `adb reboot` immediately | Settings are lost (SettingsProvider writes lazily and the reboot is abrupt) |
| `settings put`, wait about 8 s, `sync`, `adb reboot` | Settings persist (global, system and secure) |
| Provision, reboot, then save the snapshot | A later cold boot has the APK and settings, and a restore matches it |
| Image whose `ram.bin` is corrupted | The emulator exits at startup (no self-fallback). dorker detects this and cold boots |

So `dorker build` flushes for 6 s, reboots the guest, verifies every `SETTING`, and
only then saves the snapshot. This adds about 20 s to builds but makes cold boots
(corruption fallback, `--memory`/`--cpus` overrides, incompatible emulator) equivalent to restores.

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
5. If a restore fails (for example a corrupt snapshot), `dorker run` retries once with
   a cold boot and says so.

Measured with `dorker run` on this machine (API 35 `google_apis`, 2 GB, 2 CPUs,
`swiftshader_indirect`): snapshot restore 2.3-3.8 s to verified readiness across
5 runs; cold boot 14.4-19.7 s across 6 runs. These are ad hoc runs, not a
benchmark.

## Linux x86_64 validation (2026-11-05)

Ubuntu 24.04 Azure VM with nested KVM, emulator 37.2.12.0, the SDK's own `qemu-system-x86_64-headless`.

- Everything works unmodified: `doctor`, `run`, `exec`, `screenshot`, `stop`, `build` with snapshots,
  snapshot restore into a copy with the same AVD name, concurrent instances, `test`, `start`, `pull`.
  The full emulator end-to-end suite passes (312 s).
- `dorker pull android:35` really downloaded and installed `x86_64-35_r09` in 46 s (see the
  Android CLI notes below).
- ext4 has no reflinks, so `cp --reflink=auto` falls back to a full copy of the ~3-4 GB image per
  instance. XFS does clone when the filesystem was created with `reflink=1`, and so does Btrfs.
  Measured here on 2026-11-06: with the store on Azure Files (NFS) one worker took 84 s to
  start, because each worker copied the image over the network. The same image on the VM's
  local XFS disk started a worker in 11 s. Both runs are in [docs/benchmarks.md](benchmarks.md).
- SDK directories with a minor version (`android-36.1`) exist next to `android-36`; they are distinct
  images and are now modelled as such.
- **Unexplained flake:** twice (the first e2e run, and one manual run, both the first boot after
  `pull`), dorker reported "emulator exited during startup" although a QEMU for that instance was in
  fact starting. It did not recur in about ten later boots, including the first boot of a
  never-used image. The likely cause is the identity check reading the child's command line before
  `exec` finished (the SDK is on a network mount). Process state is now `Ours`/`Foreign`/`Dead`,
  read from `/proc`, and startup tolerates a pre-exec window, but I could not reproduce the failure,
  so this fix is **unproven**. The error message now includes the command line last seen.

## Network exposure (PRD 14.1), verified on macOS

With a running instance (emulator 37.2.12.0, macOS), `lsof` shows the emulator process listening
on the console port (5554), the ADB port (5555) and its ephemeral service ports **only on
`127.0.0.1` and `[::1]`**, with no UDP sockets and nothing bound to a non-loopback address.
dorker passes no option that widens this. Not yet re-checked on Linux.

## Android CLI and `sdkmanager` (checked 2026-11-05)

Google's [Android CLI](https://developer.android.com/tools/agents/android-cli) (`android`, shipped in
`cmdline-tools/latest/bin`) is the direction of travel, and dorker now prefers it.

- **Verified on two machines:** `sdkmanager` is a small shell script that forwards to
  `android sdk ...` and prints "sdkmanager is deprecated ... `android sdk` is the replacement". The
  CLI's documentation pages do not themselves say `sdkmanager` is deprecated; the notice comes from
  the tool.
- **Syntax (documented):** `android [--sdk=<root>] sdk install <package>[@<version>]`. Slash package
  paths are the new form; semicolon ids (`system-images;android-35;google_apis;arm64-v8a`) are
  "supported for backward compatibility", and the CLI translated dorker's ids when tried.
- **Exit status:** installing a package that does not exist prints `Package ... not found.` and
  exits **0**, exactly like `sdkmanager`, so `dorker pull` trusts only what is on disk afterwards.
- **Licence:** with stdin closed and no accepted licences, `android sdk install platform-tools`
  installed without a prompt and **wrote `licenses/android-sdk-license` itself**; a real arm64 system-image pull also wrote `android-sdk-arm-dbt-license`. The docs do not
  mention this. `dorker pull` therefore tells the user, with the terms URL, before it runs.
- **Telemetry (documented by Google):** command names, option names and anonymised stack traces;
  opt out with `--no-metrics`. dorker does not pass that flag, so the user's own setting applies.
- **Scope:** the CLI also manages emulators/AVDs (`android emulator ...`), installs APKs, captures
  screens and drives input. dorker does not use these: its private AVD homes, snapshot images and
  process safety are its own. Whether `android emulator` could back a future dorker runtime backend
  has not been evaluated.

## System image naming (found when `android:37` failed, 2026-11-05)

Package names cannot be guessed. Google now publishes newer APIs as **minor releases**:
`android-37.0`, `android-37.1`, `android-37.2` (and betas, canaries), not `android-37`, and some
releases publish only 16 KB-page flavours (`google_apis_ps16k`, `google_apis_playstore_ps16k`).
dorker used to build `system-images;android-37;google_apis;<abi>`, which does not exist, so
`dorker pull android:37` failed with a vague "probably does not exist". `pull` now reads
`android sdk list --all "system-images/android-<api>*"` and chooses: the base release first
(`android-37`, then `.0`, then the newest minor), then the best phone flavour (`google_apis`,
`default`, `aosp_atd`, `google_atd`, then the 16 KB variants, then Play images). Wear, TV,
tablet and automotive flavours, betas and canaries are only used when asked for explicitly or
not at all. When nothing matches, the error lists what is published. Verified on real
repositories: x86_64 Linux pulled and booted `android-37.0/google_apis` (`ro.build.version.sdk`
= 37), and the parser is tested against real `sdk list` output. If the listing cannot be read
(legacy `sdkmanager`, offline), `pull` falls back to the old guessed name.

## Pulling a missing image automatically

`dorker build` and `run` used to stop with "run `dorker pull`". They now offer to install a
missing base image (like `docker run` pulling), but only with consent: installing downloads
1 GB or more and Google's `android` CLI accepts the SDK licences on the user's behalf, so on a
terminal dorker asks first (Enter means yes), and without a terminal (CI, pipes, MCP agents) it
fails with a hint unless `--pull` / `DORKER_PULL=1` is given. Verified for real through a
pseudo-terminal on macOS: answering `n` installed nothing and wrote no licences; answering `y`
downloaded `android-35/google_apis/arm64-v8a` and completed the build (109 s).

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
4. **Host memory**: about 2.5 GB (macOS footprint) after a cold boot and 3.3 GB after a restore for a 2 GB guest; `ps` RSS is unreliable on macOS (see `benchmarks.md`).
5. **Port range** 5554-5680 follows the emulator's console convention.
6. **Image resolution** only reads installed images; downloading via
   `sdkmanager` is deferred. (Later: `dorker pull`, and `build`/`run`, can install a
   missing image with Google's `android sdk install`, after asking.)

## Open questions

- Host RAM and idle CPU per instance (needs `dorker bench`). *Answered: `benchmarks.md`.*
- `-gpu host` vs `swiftshader_indirect` boot time and screenshot correctness. *Answered:
  `benchmarks.md`; restore time is the same, but `host` uses far less CPU on a Mac, so it is
  the default there.*
- Whether `-read-only` AVD sharing could replace per-instance copies.
- Whether a snapshot is portable across machines (almost certainly not: it
  embeds host-specific state), which limits any future image registry. *Not tested; the
  design assumes it is not (see `architecture.md`).*
