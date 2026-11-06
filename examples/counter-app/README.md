# Dorker Counter

A small real Android app (Java, AGP 9.1.1, Gradle 9.3.1) used to exercise dorker during
development: a counter that persists across process death, with eleven Espresso tests.

```bash
cd examples/counter-app
# Use a locally built dorker (or just ./dev.sh with dorker on your PATH)
DORKER=../../target/debug/dorker ./dev.sh
```

`dev.sh` builds both APKs with Gradle, builds the image (`Dorkerfile`), starts an instance,
saves `counter-dev.png`, runs the instrumentation tests and removes the instance.
Needs a JDK 17+ and Android SDK platform 35. Manual steps:

```bash
./gradlew assembleDebug assembleDebugAndroidTest
dorker build -t counter:dev .
dorker run -d --name c1 counter:dev           # launches the app (CMD launch)
dorker screenshot c1 shot.png
dorker test c1 \
  --runner com.dorker.counter.test/androidx.test.runner.AndroidJUnitRunner
dorker rm -f c1
```

## Run the suite in parallel

The `Dorkerfile` installs both the app APK and the test APK, so one image holds everything
a test run needs. `dorker test --image` starts fresh instances of it, gives each one shard
of the suite, and removes them afterwards:

```bash
./gradlew assembleDebug assembleDebugAndroidTest
dorker build -t counter:dev .
dorker test --image counter:dev --workers 2 \
  --runner com.dorker.counter.test/androidx.test.runner.AndroidJUnitRunner
```

To see a failing run, build the APKs with `-PdorkerDemoFailure`: `DemoFailureTest` then fails
on purpose (it passes otherwise).

```bash
./gradlew -PdorkerDemoFailure assembleDebug assembleDebugAndroidTest
dorker build -t counter:fail .
dorker test --image counter:fail --workers 2 --runner ...   # exits 7
```

### Reading the results

Every run gets its own folder, printed at the start and the end
(`~/.dorker/results/<run-id>/`, or `<--out>/<run-id>/`):

| File | What it is |
|---|---|
| `summary.json` | the verdict, exit code, counts, per-shard status, timings and cleanup (schema `dorker.test-run/1`; `--json` prints the same object) |
| `junit.xml` | one `<testsuite>` per shard, one `<testcase>` per reported test, with measured durations; a shard that did not finish, a duplicated test or a failed cleanup is an `<error>` case named after the problem |
| `shards/<i>/result.json` | that shard's entry plus every test it ran (schema `dorker.test-shard/1`) |
| `shards/<i>/instrument.txt` | the raw `am instrument -r` output |
| `shards/<i>/logcat.txt` | the device log of the shard's run |
| `shards/<i>/emulator.log` | the emulator's own output, also for a worker that failed to boot |
| `shards/<i>/failure.png` | a screenshot taken *after* a shard did not pass; it shows the screen when the shard ended, not necessarily the failing assertion |

A diagnostic that cannot be collected in time (`DORKER_COLLECT_TIMEOUT_SECS`, default 30)
is listed under `collection_errors` and never changes the verdict.

### Things to know

- **Sharding is the runner's.** AndroidJUnitRunner splits the suite with `numShards` and
  `shardIndex`; the split is by test, so shards are uneven and a shard can be empty. Within
  a shard, tests share the instance (and its app data) in order, like a normal run; only
  the shards are isolated from each other.
- **Images are per host.** Build the image on (or for) the machine that runs it: snapshots
  depend on the emulator version, the system image and the CPU. CI should build it in the
  job (see [ci/android-tests.yml](ci/android-tests.yml)).
- **Shared backends are not isolated.** Each worker has its own device, but if tests talk to
  a server or log into an account, shards running at the same time share that. Use separate
  test accounts or data per shard where it matters.
- **Memory:** dorker budgets about 1.5x each worker's guest RAM plus 512 MB (here
  2 GB guests, so about 3.5 GB each) and keeps 2 GB for the host; it refuses to start more
  workers than fit and says how many would.
- **Disk:** keep `DORKER_HOME` on a local filesystem that can clone files: APFS, XFS
  created with `reflink=1`, or Btrfs. On NFS or ext4 every worker copies the whole image,
  one at a time, and that copy can take longer than the tests. On this machine the same
  two-worker run took 147 s from an NFS store and 57 s from local XFS.
- **More workers is not always faster.** Each emulator wants its vCPUs; past the host's
  cores, boots slow down and timing-sensitive UI tests start to fail. See
  [docs/benchmarks.md](../../docs/benchmarks.md#parallel-instrumentation).

### In CI

[ci/android-tests.yml](ci/android-tests.yml) is a GitHub Actions workflow for a Linux x86_64
runner with KVM: it installs the SDK pieces and dorker, builds the APKs and the image, runs
two shards and uploads the results folder when the run fails. Scripts can read
`summary.json` (or `--json` on stdout) rather than parsing the table.
