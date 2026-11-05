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
| Idle CPU (interval average, one instance) | 33.9 % of one core |
| Idle host RSS (emulator process) | 5.5 GB |
| Stop + remove | 1.6 s |
| Image build (`dorker build`, uncached, with persist-and-verify reboot) | about 45 s (single run; about 25 s before the reboot was added) |
| Image build (cached) | 0.45 s (single run) |

## Reading these numbers

- "Cold boot" is the official emulator started headless without snapshots, so it
  is also the baseline: dorker adds no measurable boot overhead of its own. The
  PRD's cold boot target (under 20 s) and prepared-instance target (under 5 s) are
  both met on this machine.
- **A 2 GB guest used about 5.5 GB of host memory** here. Guest RAM is not host
  memory: the figure includes the restored 2 GB snapshot, the emulator's graphics
  stack and host-side buffers. Do not size CI machines from the guest figure.
- Idle CPU stays near a third of a core with software rendering. GPU modes
  (`-gpu host`) were not benchmarked.
- Image size is about 3.3 GB logical, of which a 2 GB `ram.bin` is the snapshot.
  Instances are APFS clones of it, so each starts cheap and grows as it writes.
- Single machine, three runs, no warm-cache control. Treat as indicative.
