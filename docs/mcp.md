# Using dorker from AI agents (MCP)

`dorker mcp` runs a [Model Context Protocol](https://modelcontextprotocol.io) server on
stdio, so an AI agent can start disposable Android instances, install apps, run commands,
take screenshots, read logs and run instrumentation tests, and then clean up.

## Connect a client

Claude Code:

```bash
claude mcp add dorker -- dorker mcp
```

Any client that takes an `mcpServers` JSON block:

```json
{
  "mcpServers": {
    "dorker": { "command": "dorker", "args": ["mcp"] }
  }
}
```

Set `DORKER_HOME` or `ANDROID_HOME` in the server's `env` if you use non-default locations.
The Android SDK (emulator, platform-tools) and a system image must be installed;
`dorker_doctor` reports what is missing.

## Tools

| Tool | What it does | Result |
|---|---|---|
| `dorker_doctor` | Check SDK, emulator, adb, acceleration, disk, images | `{ok, checks[]}` |
| `dorker_ps` | List instances (`all` includes stopped) | `{instances[]}` |
| `dorker_images` | List built images | `{images[]}` |
| `dorker_build` | Build an image from a Dorkerfile (`context`, `tag`, `file`, `no_cache`) | `{image_id, build_log}` |
| `dorker_run` | Start an instance from `android:35` or an image (`name`, `memory`, `cpus`, `rm`) | instance: `{id, name, status, serial, boot, ready_secs, ...}` |
| `dorker_start` / `dorker_stop` / `dorker_rm` | Restart, stop, remove an instance | instance / `{stopped[]}` / `{removed[]}` |
| `dorker_exec` | Run a shell command in the guest | `{exit_code, stdout, stderr}` |
| `dorker_install` | Install an APK from a host path | `{ok, output}` |
| `dorker_screenshot` | Capture the screen; waits up to `wait_secs` (default 10) for a non-blank frame, so it is safe right after `dorker_run` | a PNG **image** content block |
| `dorker_logs` | Last N logcat lines (default 200) | text |
| `dorker_test` | Run an instrumentation runner | `{ok, passed, failed, errors, failures[], log_dir, ...}` |

Failures of the *guest command* (non-zero `exit_code`) and of *tests* (`ok: false`) are
returned as data. A tool that could not do its job (unknown instance, name conflict, missing
SDK, invalid arguments) returns an MCP tool error whose text is the CLI's message plus its
exit-code meaning, so the model can read it and recover.

Every tool maps to a CLI command with `--json`, so you can reproduce a call by hand, e.g.
`dorker exec --json agent "pm list packages"`.

## A typical session

1. `dorker_doctor`: confirm the host is ready.
2. `dorker_build` (once per project) then `dorker_run` with the image, or `dorker_run`
   with `android:35` for a clean device.
3. `dorker_install`, `dorker_exec`, `dorker_screenshot`, `dorker_logs`, `dorker_test`.
4. `dorker_rm` when finished. Instances persist between calls and keep running until removed.

## Security notes

The server runs with your user's privileges and is meant for local, trusted agents:

- A connected agent can run arbitrary commands **inside the Android guest**, and can read
  host files named in `dorker_install` (`apk_path`) and `dorker_build` (`context`).
- It never runs a host shell. Arguments are passed as an argument vector, and values for
  instance and image names that start with `-` are rejected so they cannot become flags.
- Screenshots never touch a temporary file: the PNG is read from the CLI's stdout (`dorker screenshot <instance> -`), so there is no predictable path for another local user to pre-create.
- ADB is not exposed on the network; instances use the emulator's local console and adb ports.
- Tool calls run as separate `dorker` processes. Long calls (a build) do not block other requests.

## Limits

- Screenshots are returned at full resolution (about 1 MB base64 for 1080x2400). Some clients
  downscale; dorker does not.
- There is no cancellation of an in-flight tool call yet.
- Tools do not stream progress. `dorker_build` and a cold `dorker_run` return only when done.
