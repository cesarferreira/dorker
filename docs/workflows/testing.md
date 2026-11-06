# Testing

`dorker test` runs an instrumentation test runner inside an instance. It prints a summary and exits 7 if anything failed. It keeps the raw instrumentation output and logcat inside the instance, which `dorker rm` deletes, so add `--out test-results` to copy them somewhere that survives (for CI artifacts).

```bash
dorker test t1 \
  --runner com.example.test/androidx.test.runner.AndroidJUnitRunner
dorker test t1 --runner ... --class com.example.FooTest#bar
```

A runnable example is in [examples/instrumentation](https://github.com/cesarferreira/dorker/tree/main/examples/instrumentation).

## Shard a suite across disposable emulators

```bash
dorker test --image myapp:ci --workers 4 \
  --runner com.example.test/androidx.test.runner.AndroidJUnitRunner
```

With `--image`, dorker starts `--workers` fresh instances of a built image (APKs installed by the Dorkerfile), runs shard `i` of `N` on worker `i` with AndroidJUnitRunner's `-e numShards N -e shardIndex i`, and removes every worker it started, also on failure, Ctrl-C, or `SIGTERM`. It never touches instances it did not start.

Before starting, it checks memory, disk, and free ports for all workers. If they do not fit it refuses with exit 8 and says how many workers would fit. `DORKER_HOST_RESERVE_MB` (default 2048) is the memory it leaves for the host. The check is an estimate. Pass `--force` to start the workers anyway: the failed check is printed as a warning, and the host may run low on memory.

The run passes only if every shard ran to its final result, nothing failed, at least one test ran, no test was reported by two shards (a runner that ignores the shard arguments), and cleanup succeeded.

| Exit | Meaning |
|---|---|
| 0 | every shard passed |
| 5 | a worker failed to start |
| 7 | a test failed, or the sharded run was incomplete |
| 8 | not enough capacity for `--workers` |
| 130 | the run was interrupted (its workers are removed) |

Results go to `~/.dorker/results/<run-id>/` (or `--out <dir>/<run-id>/`) and outlive the workers:

- `summary.json` (schema `dorker.test-run/1`, also printed by `--json`)
- `junit.xml`
- `shards/<i>/` with `result.json`, `instrument.txt`, `logcat.txt`, `emulator.log`, and, for a shard that did not pass, `failure.png`

`--class` applies to every shard. `--startup-timeout` (default 300s) bounds each worker's wait for its turn to clone the image and, separately, its boot. `--timeout` bounds each shard's tests. On a filesystem that can clone, that wait is short. On NFS or ext4 the workers copy the image one at a time, and for a short suite that copy costs more than the tests. Android Test Orchestrator is not supported in this mode.

[examples/counter-app](https://github.com/cesarferreira/dorker/tree/main/examples/counter-app#run-the-suite-in-parallel) walks through a real app, the report files, and a CI workflow. [Benchmarks](../benchmarks.md#parallel-instrumentation) has the measurements. The MCP server keeps running tests in one instance.

## Measure an image

```bash
dorker bench myapp:latest --runs 3 --json
dorker bench android:35 --baseline
```

`dorker bench` reports boot, restore, launch, disk, and idle numbers. `--baseline` also times the plain official emulator, for comparison. It applies to base `android:<api>` images only.

Next: [Disk and cleanup →](disk.md)
