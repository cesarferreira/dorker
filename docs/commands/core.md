# Core commands

Day-to-day commands. Flags for a command are in `dorker <command> --help`. Most of these accept `--json`.

## Host and images

| Command | What it does |
|---|---|
| `dorker doctor` | Check the SDK, emulator, adb, acceleration, and disk |
| `dorker versions` | List Android versions you can use (installed and available to pull) |
| `dorker pull android:35` | Install a system image. With no argument, install the `FROM` image of `./Dorkerfile` |
| `dorker build -t myapp:latest .` | Build an image from a Dorkerfile |
| `dorker images` | List built images |
| `dorker rmi <image>` | Remove an image |
| `dorker ls` | Images, instances, and storage. Alias: `dorker df` |
| `dorker prune` | Remove images you no longer need |

`dorker build` and `dorker run` offer to install a missing system image. Pass `--pull` (or set `DORKER_PULL=1`) to install without asking. A Dorkerfile's `FETCH` and `PROVISION` scripts run automatically without a confirmation prompt, including in CI. They execute code on your machine, so only build trusted Dorkerfiles and scripts. The MCP server refuses to run them.

## Instances

| Command | What it does |
|---|---|
| `dorker run -d --name t1` | Build the Dorkerfile in this folder (a cache hit when nothing changed) and start it |
| `dorker run -d --name t1 android:35` | Start a plain system image |
| `dorker run -d --name t1 myapp:latest` | Start a built image by name |
| `dorker run --rm` | Run in the foreground and remove the instance on Ctrl-C |
| `dorker run --logs` | Also stream logcat |
| `dorker ps` | List instances |
| `dorker exec t1 "getprop ro.build.version.sdk"` | Run a shell command in the guest. The exit status is the guest command's |
| `dorker install t1 ./app.apk` | Install an APK into a running instance |
| `dorker screenshot t1 shot.png` | Capture a PNG. Waits up to 10s for a non-blank frame |
| `dorker logs t1` | Show logcat. Add `-f` to follow |
| `dorker stop t1` | Stop one instance |
| `dorker stop --all` | Stop every running instance |
| `dorker stop --orphans` | Stop emulators dorker lost track of (asks first; `--yes` skips the question) |
| `dorker start t1` | Cold boot a stopped instance. Keeps its data |
| `dorker rm t1` | Remove an instance and its data. `-f` does not ask |
| `dorker commit setup -t myapp:ready` | Save a running instance as a new image |

`dorker ps` warns about an orphan. `--orphans` asks before stopping, because such an emulator may belong to another `DORKER_HOME` that is still using it.

## Tests and measurements

| Command | What it does |
|---|---|
| `dorker test t1 --runner …` | Run instrumentation tests in an existing instance |
| `dorker test --image myapp:ci --workers 4 --runner …` | Shard a suite across disposable instances |
| `dorker bench myapp:latest --runs 3` | Measure boot, restore, launch, disk, and idle |
| `dorker bench android:35 --baseline` | Also time the plain official emulator |

`dorker test` prints a summary and exits 7 if anything failed. See [Testing](../workflows/testing.md).

## Project files

| Command | What it does |
|---|---|
| `dorker templates` | List templates, or show one (`dorker templates android-auto`) |
| `dorker init --template screenshots` | Copy a template into this folder |
| `dorker config` | Show each setting in effect and where it came from |
| `dorker config init` | Write a commented example config |
| `dorker mcp` | Model Context Protocol server for AI agents |

`dorker -v <command>` (or `DORKER_VERBOSE=1`) prints what dorker decides and runs, on stderr: the located SDK, the boot mode and why, the exact emulator command line and environment, state transitions, and timings. Each instance also keeps its raw emulator output in `~/.dorker/instances/<id>/emulator.log`.

`doctor`, `ls`, `ps`, `images`, `versions`, `prune`, `config`, and `templates` colour their output on a terminal (green ok, yellow warn, bold red FAIL, fixes and IDs in cyan, sizes in yellow). Colour is off when piped. `NO_COLOR` turns it off, `CLICOLOR_FORCE=1` forces it on, and `--json` is never coloured.

Exit codes are listed in [Limitations](../reference/limitations.md#exit-codes).

## Next

- [Build and run](../workflows/build-and-run.md)
- [Testing](../workflows/testing.md)
- [Dorkerfile](../dorkerfile.md)
