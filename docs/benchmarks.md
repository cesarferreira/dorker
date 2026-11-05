# Benchmarks

Produced with `dorker bench sample --runs 3 --apk examples/instrumentation/test.apk`
on 2026-11-05. Reproduce with the same command; add `--json` for machine-readable output.

| | |
|---|---|
| Machine | macOS 26.7.2, Apple M4 Pro, 14 cores, 24 GB |
| Emulator | 37.2.12.0 |
| Image | `examples/basic` (API 35 `google_apis`, 2 GB, 2 CPUs, `swiftshader_indirect`, headless) |

| Metric | Result (median, samples) |
|---|---|
| Cold boot to verified readiness | 15.1 s (15.1, 15.8, 14.5) |
| Snapshot restore to verified readiness | 3.8 s (3.8, 2.9, 3.8) |
| APK install (instrumentation test APK) | 1.3 s |
| Screenshot (1080x2400 PNG) | 1.3 s |
| Idle CPU, after the guest settles | 18-36 % of one core (noisy; see "Idle CPU and memory" below) |
| Host memory (physical footprint) | about 2.2-2.6 GB after a cold boot, 3.2-3.4 GB after a snapshot restore |
| Stop + remove | 1.6 s |
| Image build (`dorker build`, uncached, with persist-and-verify reboot) | about 45 s (single run; about 25 s before the reboot was added) |
| Image build (cached) | 0.45 s (single run) |

## Reading these numbers

- "Cold boot" is the official emulator started headless without snapshots, so it
  is also the baseline: dorker adds no measurable boot overhead of its own. The
  PRD's cold boot target (under 20 s) and prepared-instance target (under 5 s) are
  both met on this machine.
- **Corrected:** this file originally reported "idle CPU 33.9 %" and "idle host RSS 5.5 GB".
  Both were misleading. The CPU figure was taken 10 s after the guest was ready, while
  Android was still doing first-boot work; the memory figure was `ps` RSS, which on macOS
  excludes compressed pages and varied from 0.8 GB to 3.6 GB for the *same* workload.
  `dorker bench` now waits for the guest to go quiet before measuring CPU and reports
  physical footprint on macOS (see below).
- Image size is about 3.3 GB logical, of which a 2 GB `ram.bin` is the snapshot.
  Instances are APFS clones of it, so each starts cheap and grows as it writes.
- Single machine, a few runs, no warm-cache control. Treat as indicative.

## Baselines, overhead, disk and app launch

`dorker bench android:35 --runs 3 --baseline` and `dorker bench counter:bench --runs 3`
(the real Gradle app image, `examples/counter-app`) on the Mac, 2026-11-05. Medians in seconds.

| Measurement | Result | Notes |
|---|---|---|
| Official emulator, cold, no snapshot (baseline 1) | 18.0 (17.8, 18.0, 18.3) | fresh AVD each run, bare `boot_completed` poll, no dorker orchestration |
| dorker cold boot, base image (baseline 3) | 17.5 (17.4, 17.5, 18.8) | same emulator and flags; **no measurable dorker overhead** |
| Official emulator with its own quickboot (baseline 2) | 2.8 (3.2, 2.2, 2.8) | `default_boot`, restored on the 2nd+ boot of the same AVD |
| dorker prepared image, snapshot restore (baseline 4) | 3.9 (4.0, 3.9, 3.7) | 3.5 GB image with the app installed |
| App launch to first frame (`am start -W` + splash gone) | 1.2 | counter app |
| Screenshot | 0.2 | |
| Stop + remove | 1.6 | |

- **Where dorker's 3.7 s restore goes:** about 0.1-0.3 s of dorker work before the emulator is
  spawned (checks and the clone, which is about 20 ms), then about 3.6 s until adb sees the
  device, and the app is ready about 60 ms after that. So the roughly 1 s gap to the plain
  emulator's quickboot (2.8 s) is the emulator's own snapshot load, not dorker's overhead.
  Tested: a fresh clone's 2.5 GB `ram.bin` takes 0.40-0.44 s to read the first time (a clone
  does not share the page cache with its source) against 0.14-0.17 s when cached, so cache
  misses account for about 0.25-0.3 s of the gap. The remaining ~0.7 s is unexplained. Both
  meet the PRD's under-5-second target for a prepared instance.
- **Disk:** a new instance of the 3.5 GB image costs **+19 MB** of physical storage (measured as
  the change in free space, which accounts for copy-on-write clones) against 3.5 GB logical.
  That is APFS cloning; on filesystems without clones (ext4) it is a full copy. An unprovisioned
  `android:35` instance occupies about 0.8 GB after boot.

## Idle CPU and memory

Measured by `dorker bench` after the emulator process uses under 25 % of a core over a
5 s window (up to 90 s of waiting), then averaged over 10 s.

- **Memory:** on the Mac, physical footprint is about 2.2-2.6 GB after a cold boot and
  3.2-3.4 GB after a snapshot restore. A restore therefore costs about 1 GB more host
  memory than a cold boot of the same guest, because the restored RAM image is resident.
  On Linux (VmRSS) it is 3.0-3.5 GB cold and 3.4-3.7 GB restored.
- **CPU:** about 10 % of one core on a fully warmed guest (Linux, see below), but
  18-36 % right after boot on the Mac, and **one restored instance never went quiet in
  90 s (130 %)** while another settled in 55 s. Idle CPU is dominated by Android and
  Google apps doing background work, not by rendering, and it varies a lot between runs.
  If you need a low-CPU idle instance, expect to wait about a minute after start.

## GPU modes

Compared with identical images that differ only in the `GPU` directive, interleaved
(A, B, A, B), 4 runs per round. Medians in seconds.

| Platform | Mode | Cold boot | Snapshot restore | Notes |
|---|---|---|---|---|
| macOS M4 Pro | `swiftshader_indirect` (default) | 17.0, 16.8 | 3.6, 3.9 | |
| macOS M4 Pro | `host` | 16.6, 12.6 | 4.2, 4.4 | restore is consistently about 0.4 s slower; screenshots faster (1.2-1.5 s vs 2.0-2.2 s) |
| Linux x86_64 | `swiftshader_indirect` (default) | 29.1, 29.1 | 8.6, 9.2 | |
| Linux x86_64 | `angle_indirect` | 25.6, 26.5 | 9.1, 10.2 | cold boot about 11 % faster, restore noisy and not faster; about 0.3-0.4 GB less memory |
| Linux x86_64 | `host` | fails | n/a | "Could not start renderer": no GPU or display on a headless VM |

Warmed-guest control on Linux (60 s settle, same AVD, `-gpu` on the emulator command line):
`swiftshader_indirect` 9.9 % and 12.6 %, `guest` 9.8 %, `angle_indirect` 9.7 % and 15.2 %,
`off` 10.9 % idle CPU. The spread within one mode is as large as the spread between modes.

**Conclusion:** no GPU mode improved snapshot restore, which is the path dorker is built
around, so the default stays `swiftshader_indirect` (it works on every host). `GPU` is
available as an opt-in per Dorkerfile; `host` needs a real GPU and display, and
`angle_indirect` may help cold-boot-heavy or memory-constrained Linux setups. The GPU
mode is part of the snapshot's hardware identity, so it is part of the image fingerprint.

### Interactive use: UI smoothness (macOS)

Restore time is not the whole story when someone is *watching* the device (for example
with scrcpy). Scrolling Settings on two images that differ only in `GPU`, M4 Pro, API 35:

| Mode | Host CPU while scrolling | Guest frame time (median / 90th) |
|---|---|---|
| `swiftshader_indirect` (default) | about 600 % (six cores) | 23 / 24 ms |
| `host` | about 43 % | 19 / 23 ms |

On the Linux VM (8 vCPU Azure, no GPU, `hyperv_drm`) the same scrolling test gave
`swiftshader_indirect` about 650 % and `angle_indirect` about 610 % host CPU (and 0.3 GB
less memory), so both are CPU-bound there. `host` fails to start the renderer, and `auto`
on a GPU-less host booted in 68 s (against about 29 s) and gave a near-blank first
screenshot, so Linux keeps `swiftshader_indirect`. A Linux machine with a real GPU can
opt in with `GPU host`.

Single measurement each, so treat the ratio as indicative. Because of this, macOS now
defaults to `host`; other platforms keep `swiftshader_indirect`, which works on GPU-less
hosts such as headless CI. Restore is about 0.4 s slower with `host` (table above).

## Linux x86_64 (KVM)

Same command (`dorker bench sample --runs 3 --apk examples/instrumentation/test.apk`) on
2026-11-05, image from `examples/basic`.

| | |
|---|---|
| Machine | Ubuntu 24.04, AMD EPYC 7763, 8 vCPUs, 31 GB, **Azure VM (nested virtualisation, `/dev/kvm`)** |
| Emulator | 37.2.12.0 (same build as on the Mac) |
| Image | API 35 `google_apis` **x86_64**, 2 GB, 2 CPUs, `swiftshader_indirect`, headless |

| Metric | Result (median, samples) |
|---|---|
| Cold boot to verified readiness | 28.8 s (28.3, 28.8, 29.2) |
| Snapshot restore to verified readiness | 9.2 s (9.2, 9.3, 9.0) |
| APK install | 4.3 s |
| Screenshot | 2.0 s |
| Idle CPU | **229 % was a measurement artifact** (guest first-boot work); about 10 % once warmed up, 15-60 % in later runs, noisy |
| Host memory (VmRSS) | 3.5 GB cold, 3.7 GB after restore |
| Stop + remove | 2.2 s |
| Image build (uncached) | 92 s |

Notes: this is a virtual machine on shared infrastructure with nested virtualisation, so
treat it as a pessimistic CI-like environment, not bare-metal Linux. Boots were slower
(46-65 s) the first time an image was read from disk, and these numbers are from a warm
page cache. The Mac's restore target (under 5 s) is **not** met here (9.2 s). Three runs, one machine. See "Idle CPU and memory" above for why the idle CPU figure
originally listed here was wrong.
