//! Integration tests that drive the real binary. The first group needs no
//! emulator. The `#[ignore]` group needs an Android SDK with an installed system
//! image and boots real emulators: run it with `cargo test -- --ignored`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Env {
    home: PathBuf,
}

impl Env {
    fn new(name: &str) -> Self {
        let home = std::env::temp_dir().join(format!("dorker-it-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        Env { home }
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_dorker"));
        c.args(args).env("DORKER_HOME", &self.home);
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd(args).output().unwrap()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        // Best effort: stop anything a failed test left running.
        if let Ok(out) = Command::new(env!("CARGO_BIN_EXE_dorker"))
            .args(["ps", "-a", "--json"])
            .env("DORKER_HOME", &self.home)
            .output()
        {
            if let Ok(rows) = serde_json::from_slice::<Vec<serde_json::Value>>(&out.stdout) {
                for r in rows {
                    let _ = Command::new(env!("CARGO_BIN_EXE_dorker"))
                        .args(["rm", "-f", r["id"].as_str().unwrap_or("")])
                        .env("DORKER_HOME", &self.home)
                        .output();
                }
            }
        }
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or(-1)
}

fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, body).unwrap();
    p
}

#[test]
fn empty_state_lists_nothing() {
    let e = Env::new("empty");
    let ps = e.run(&["ps", "-a"]);
    assert_eq!(code(&ps), 0);
    assert_eq!(text(&ps.stdout).lines().count(), 1, "header only");
    assert_eq!(code(&e.run(&["images"])), 0);
    assert_eq!(text(&e.run(&["ps", "--json"]).stdout).trim(), "[]");
}

#[test]
fn unknown_instance_and_image_are_errors() {
    let e = Env::new("unknown");
    let o = e.run(&["stop", "nope"]);
    assert_eq!(code(&o), 1);
    assert!(text(&o.stderr).contains("no such instance"));
    assert_eq!(code(&e.run(&["rmi", "nope"])), 1);
}

#[test]
fn dorkerfile_errors_use_exit_code_2_and_show_position() {
    let e = Env::new("parse");
    let ctx = e.home.join("ctx");
    std::fs::create_dir_all(&ctx).unwrap();
    write(&ctx, "Dorkerfile", "FROM android:35\n\nMEM 2GB\n");
    let o = e.run(&["build", ctx.to_str().unwrap()]);
    assert_eq!(code(&o), 2);
    let err = text(&o.stderr);
    assert!(
        err.starts_with("Dorkerfile:3:1\nerror: unsupported directive \"MEM\""),
        "{err}"
    );
    assert!(err.contains("Did you mean \"MEMORY\"?"));
    assert_eq!(
        err.matches("error:").count(),
        1,
        "no duplicated prefix: {err}"
    );
}

#[test]
fn missing_sdk_is_exit_code_3() {
    let e = Env::new("nosdk");
    let o = e
        .cmd(&["run", "-d", "android:35"])
        .env("ANDROID_HOME", "/nonexistent")
        .env("ANDROID_SDK_ROOT", "/nonexistent")
        .env("HOME", "/nonexistent")
        .output()
        .unwrap();
    assert_eq!(code(&o), 3, "{}", text(&o.stderr));
    assert!(text(&o.stderr).contains("Android SDK not found"));
}

#[test]
fn invalid_arguments_are_exit_code_2() {
    let e = Env::new("args");
    // These fail validation before anything is started, with or without an SDK.
    let o = e.run(&["run", "-d", "android:35", "--memory", "banana"]);
    assert!(matches!(code(&o), 2 | 3), "{}", text(&o.stderr));
    let o = e.run(&["test", "x", "--runner", "bad; reboot"]);
    assert_eq!(code(&o), 2);
}

#[test]
fn insufficient_disk_space_is_a_resource_conflict_before_anything_is_created() {
    let e = Env::new("disk");
    // An absurd requirement makes the preflight fail on any machine, with no SDK needed.
    let o = e
        .cmd(&["run", "-d", "android:35"])
        .env("DORKER_MIN_FREE_MB", "999999999999")
        .output()
        .unwrap();
    assert_eq!(code(&o), 8, "{}", text(&o.stderr));
    let err = text(&o.stderr);
    assert!(err.contains("not enough free disk space"), "{err}");
    assert!(
        err.contains("DORKER_MIN_FREE_MB"),
        "tells the user how to adjust it: {err}"
    );
    assert_eq!(
        std::fs::read_dir(e.home.join("instances")).unwrap().count(),
        0,
        "nothing created"
    );
}

/// Send newline-delimited JSON-RPC messages to `dorker mcp`, close stdin, return the responses by id.
fn mcp_session(
    e: &Env,
    messages: &[serde_json::Value],
    raw_extra: &[&str],
) -> Vec<serde_json::Value> {
    use std::io::Write;
    use std::process::Stdio;
    let mut child = e
        .cmd(&["mcp"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    {
        let stdin = child.stdin.as_mut().unwrap();
        for m in messages {
            writeln!(stdin, "{m}").unwrap();
        }
        for r in raw_extra {
            writeln!(stdin, "{r}").unwrap();
        }
    }
    drop(child.stdin.take()); // EOF: the server answers in-flight calls, then exits
    let out = child.wait_with_output().unwrap();
    assert_eq!(
        code(&Output {
            status: out.status,
            stdout: vec![],
            stderr: vec![]
        }),
        0,
        "server exits cleanly on EOF"
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| {
            serde_json::from_str(l)
                .unwrap_or_else(|e| panic!("stdout must carry only JSON-RPC, got {l:?}: {e}"))
        })
        .collect()
}

#[test]
fn mcp_server_speaks_the_protocol_over_stdio() {
    use serde_json::json;
    let e = Env::new("mcp");
    let rpc = |id: u32, method: &str, params: serde_json::Value| json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
    let msgs = [
        rpc(
            1,
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}),
        ),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        rpc(2, "tools/list", json!({})),
        rpc(
            3,
            "tools/call",
            json!({"name": "dorker_ps", "arguments": {"all": true}}),
        ),
        rpc(
            4,
            "tools/call",
            json!({"name": "dorker_run", "arguments": {}}),
        ),
        rpc(
            5,
            "tools/call",
            json!({"name": "dorker_exec", "arguments": {"instance": "nope", "command": "ls"}}),
        ),
        rpc(
            6,
            "tools/call",
            json!({"name": "dorker_doctor", "arguments": {}}),
        ),
        rpc(7, "no/such/method", json!({})),
    ];
    let out = mcp_session(&e, &msgs, &["this is not json"]);
    let by_id = |id: u64| {
        out.iter()
            .find(|m| m["id"] == id)
            .unwrap_or_else(|| panic!("no response for id {id}: {out:?}"))
    };

    assert_eq!(by_id(1)["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(by_id(1)["result"]["serverInfo"]["name"], "dorker");
    let tools = by_id(2)["result"]["tools"].as_array().unwrap().clone();
    assert!(tools.len() >= 12, "{} tools", tools.len());
    // No response to the notification, and exactly one per request.
    assert!(out.iter().filter(|m| m["id"].is_u64()).count() == 7);

    // dorker_ps: structured {"instances": []} on an empty store.
    assert_eq!(by_id(3)["result"]["isError"], false);
    assert_eq!(
        by_id(3)["result"]["structuredContent"]["instances"],
        json!([])
    );
    // Missing argument: a tool error the model can read, not a protocol error.
    assert_eq!(by_id(4)["result"]["isError"], true);
    assert!(
        by_id(4)["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("missing required argument `image`")
    );
    // Unknown instance: the CLI's own error comes back as a tool error.
    assert_eq!(by_id(5)["result"]["isError"], true);
    assert!(
        by_id(5)["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("no such instance")
    );
    // doctor reports problems inside the JSON (no SDK needed to get an answer).
    assert!(by_id(6)["result"]["structuredContent"]["ok"].is_boolean());
    assert_eq!(by_id(7)["error"]["code"], -32601);
    // The garbage line produced a parse error with a null id, and did not kill the server.
    assert!(
        out.iter()
            .any(|m| m["id"].is_null() && m["error"]["code"] == -32700),
        "{out:?}"
    );
}

#[test]
fn json_flags_report_failures_as_json_or_plain_errors() {
    let e = Env::new("jsonflags");
    // run --json without --detach is a configuration error, before anything starts.
    let o = e.run(&["run", "--json", "android:35"]);
    assert_eq!(code(&o), 2, "{}", text(&o.stderr));
    assert!(text(&o.stderr).contains("--json needs --detach"));
    // exec on an unknown instance is a plain error (no JSON on stdout).
    let o = e.run(&["exec", "--json", "nope", "ls"]);
    assert_eq!(code(&o), 1);
    assert!(text(&o.stdout).trim().is_empty());
}

#[test]
fn init_creates_a_parseable_dorkerfile_and_never_overwrites() {
    let e = Env::new("init");
    let dir = e.home.join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let o = e.cmd(&["init"]).current_dir(&dir).output().unwrap();
    assert_eq!(code(&o), 0);
    let body = std::fs::read_to_string(dir.join("Dorkerfile")).unwrap();
    assert!(body.starts_with("FROM android:35"));
    let again = e.cmd(&["init"]).current_dir(&dir).output().unwrap();
    assert_eq!(code(&again), 8, "refuses to overwrite");
    assert_eq!(
        std::fs::read_to_string(dir.join("Dorkerfile")).unwrap(),
        body
    );
}

// ---------- emulator end-to-end (ignored by default) ----------

fn example_apks() -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/instrumentation");
    if !dir.join("test.apk").exists() {
        let st = Command::new(dir.join("make-apk.sh"))
            .status()
            .expect("make-apk.sh");
        assert!(
            st.success(),
            "building example APKs needs the Android build-tools and a JDK"
        );
    }
    dir
}

fn adb_ok(e: &Env, args: &[&str]) -> String {
    let o = e.run(args);
    assert_eq!(code(&o), 0, "{args:?} failed: {}", text(&o.stderr));
    text(&o.stdout)
}

#[test]
#[ignore = "boots real emulators; run with --ignored"]
fn end_to_end_build_run_exec_screenshot_test_cleanup() {
    let e = Env::new("e2e");
    let ctx = example_apks();
    let ctx = ctx.to_str().unwrap();

    // Build, then an identical rebuild must hit the cache.
    let b = e.run(&["build", "-t", "e2e:test", ctx]);
    assert_eq!(code(&b), 0, "{}", text(&b.stderr));
    let id = text(&b.stdout).trim().to_string();
    let again = e.run(&["build", "-t", "e2e:again", ctx]);
    assert!(
        text(&again.stderr).contains("Using cached image"),
        "{}",
        text(&again.stderr)
    );
    assert_eq!(text(&again.stdout).trim(), id);
    assert!(Path::new(ctx).join("Dorkerfile.lock").is_file());

    // Two instances at once, restored from the snapshot, with distinct ports.
    adb_ok(&e, &["run", "-d", "--name", "a", "e2e:test"]);
    adb_ok(&e, &["run", "-d", "--name", "b", "e2e:test"]);
    let ps: Vec<serde_json::Value> =
        serde_json::from_slice(&e.run(&["ps", "--json"]).stdout).unwrap();
    assert_eq!(ps.len(), 2);
    assert_ne!(ps[0]["serial"], ps[1]["serial"]);
    assert!(ps.iter().all(|r| r["status"] == "READY"));

    // exec reaches the right instance and propagates exit codes.
    assert_eq!(
        adb_ok(&e, &["exec", "a", "getprop ro.build.version.sdk"]).trim(),
        "35"
    );
    assert!(adb_ok(&e, &["exec", "a", "pm list packages"]).contains("com.dorker.sample"));
    assert_eq!(code(&e.run(&["exec", "a", "sh -c 'exit 7'"])), 7);

    // Writable state is isolated per instance.
    adb_ok(&e, &["exec", "a", "touch /data/local/tmp/only_in_a"]);
    assert!(!adb_ok(&e, &["exec", "b", "ls /data/local/tmp"]).contains("only_in_a"));

    // Screenshot is a real PNG.
    let png = e.home.join("shot.png");
    adb_ok(&e, &["screenshot", "a", png.to_str().unwrap()]);
    assert!(
        std::fs::read(&png)
            .unwrap()
            .starts_with(b"\x89PNG\r\n\x1a\n")
    );

    // Instrumentation: failures exit 7, a passing selection exits 0.
    let runner = "com.dorker.sample.test/com.dorker.sample.SampleRunner";
    let all = e.run(&["test", "a", "--runner", runner]);
    assert_eq!(code(&all), 7);
    assert!(text(&all.stdout).contains("2 passed, 1 failed"));
    let ok = e.run(&[
        "test",
        "a",
        "--runner",
        runner,
        "--class",
        "com.dorker.sample.SampleTests#passing",
    ]);
    assert_eq!(code(&ok), 0, "{}", text(&ok.stdout));

    // A cold boot of the image has the same provisioned state as a snapshot restore.
    adb_ok(
        &e,
        &["run", "-d", "--name", "cold", "--memory", "3GB", "e2e:test"],
    );
    assert!(adb_ok(&e, &["exec", "cold", "pm list packages"]).contains("com.dorker.sample"));

    // A stopped instance can be restarted and keeps the data it wrote.
    adb_ok(&e, &["exec", "cold", "echo kept > /data/local/tmp/marker"]);
    adb_ok(&e, &["stop", "cold"]);
    assert_eq!(
        code(&e.run(&["start", "a"])),
        8,
        "starting a running instance is a conflict"
    );
    adb_ok(&e, &["start", "cold"]);
    assert_eq!(
        adb_ok(&e, &["exec", "cold", "cat /data/local/tmp/marker"]).trim(),
        "kept"
    );

    // Name collisions are a conflict.
    assert_eq!(code(&e.run(&["run", "-d", "--name", "a", "e2e:test"])), 8);

    // Stop/remove clean up; the image is untouched.
    assert_eq!(code(&e.run(&["rm", "a"])), 1, "running instance needs -f");
    adb_ok(&e, &["rm", "-f", "a", "b", "cold"]);
    assert_eq!(text(&e.run(&["ps", "-a", "--json"]).stdout).trim(), "[]");
    assert_eq!(
        std::fs::read_dir(e.home.join("instances")).unwrap().count(),
        0
    );
    assert!(text(&e.run(&["images"]).stdout).contains("e2e"));
}

#[test]
#[ignore = "boots real emulators; run with --ignored"]
fn failed_build_leaves_nothing_behind() {
    let e = Env::new("badbuild");
    let ctx = e.home.join("ctx");
    std::fs::create_dir_all(&ctx).unwrap();
    write(
        &ctx,
        "Dorkerfile",
        "FROM android:35\nINSTALL ./bad.apk\nSNAPSHOT ready\n",
    );
    write(&ctx, "bad.apk", "not an apk");
    let o = e.run(&["build", "-t", "bad", ctx.to_str().unwrap()]);
    assert_eq!(code(&o), 4, "{}", text(&o.stderr));
    assert!(text(&o.stderr).contains("INSTALL_PARSE_FAILED"));
    assert_eq!(text(&e.run(&["ps", "-a", "--json"]).stdout).trim(), "[]");
    assert_eq!(
        std::fs::read_dir(e.home.join("instances")).unwrap().count(),
        0
    );
    let images: Vec<_> = std::fs::read_dir(e.home.join("images")).unwrap().collect();
    assert!(images.is_empty(), "no partial image published");
    assert!(!ctx.join("Dorkerfile.lock").exists());
}

/// A sequential MCP client: send one request, read until its response arrives.
struct McpClient {
    child: std::process::Child,
    reader: std::io::BufReader<std::process::ChildStdout>,
    next_id: u64,
}

impl McpClient {
    fn start(e: &Env) -> Self {
        use std::process::Stdio;
        let mut child = e
            .cmd(&["mcp"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let reader = std::io::BufReader::new(child.stdout.take().unwrap());
        McpClient {
            child,
            reader,
            next_id: 0,
        }
    }

    fn call(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        use std::io::{BufRead, Write};
        self.next_id += 1;
        let id = self.next_id;
        let msg =
            serde_json::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(self.child.stdin.as_mut().unwrap(), "{msg}").unwrap();
        loop {
            let mut line = String::new();
            assert!(
                self.reader.read_line(&mut line).unwrap() > 0,
                "server closed stdout"
            );
            let v: serde_json::Value =
                serde_json::from_str(&line).expect("stdout carries only JSON-RPC");
            if v["id"] == id {
                return v;
            }
        }
    }

    fn tool(&mut self, name: &str, args: serde_json::Value) -> serde_json::Value {
        self.call(
            "tools/call",
            serde_json::json!({"name": name, "arguments": args}),
        )["result"]
            .clone()
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        drop(self.child.stdin.take());
        let _ = self.child.wait();
    }
}

#[test]
#[ignore = "boots real emulators; run with --ignored"]
fn mcp_agent_workflow_against_a_real_emulator() {
    use serde_json::json;
    let e = Env::new("mcp-e2e");
    let ctx = example_apks();
    let mut c = McpClient::start(&e);
    c.call("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "e2e", "version": "0"}}));

    let built = c.tool(
        "dorker_build",
        json!({"context": ctx.to_str().unwrap(), "tag": "mcp:e2e"}),
    );
    assert_eq!(built["isError"], false, "{built}");
    let run = c.tool("dorker_run", json!({"image": "mcp:e2e", "name": "agent"}));
    assert_eq!(run["isError"], false, "{run}");
    assert_eq!(run["structuredContent"]["status"], "READY");
    assert_eq!(run["structuredContent"]["boot"], "snapshot");

    // exec returns the guest's exit code and streams separately; a failure is data, not an error.
    let ok = c.tool(
        "dorker_exec",
        json!({"instance": "agent", "command": "getprop ro.build.version.sdk"}),
    );
    assert_eq!(
        ok["structuredContent"]["stdout"].as_str().unwrap().trim(),
        "35"
    );
    let bad = c.tool(
        "dorker_exec",
        json!({"instance": "agent", "command": "sh -c 'echo oops >&2; exit 3'"}),
    );
    assert_eq!(bad["isError"], false);
    assert_eq!(bad["structuredContent"]["exit_code"], 3);
    assert_eq!(
        bad["structuredContent"]["stderr"].as_str().unwrap().trim(),
        "oops"
    );

    // Screenshots come back as an image content block holding a PNG.
    let shot = c.tool("dorker_screenshot", json!({"instance": "agent"}));
    let img = shot["content"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["type"] == "image")
        .expect("image block");
    assert_eq!(img["mimeType"], "image/png");
    assert!(
        img["data"].as_str().unwrap().starts_with("iVBORw0KGgo"),
        "base64 of the PNG signature"
    );

    // Test results are structured: counts plus the failing test's stack trace.
    let runner = "com.dorker.sample.test/com.dorker.sample.SampleRunner";
    let t = c.tool(
        "dorker_test",
        json!({"instance": "agent", "runner": runner}),
    );
    assert_eq!(t["structuredContent"]["ok"], false);
    assert_eq!(t["structuredContent"]["passed"], 2);
    assert_eq!(t["structuredContent"]["failures"][0]["test"], "failing");
    let t = c.tool("dorker_test", json!({"instance": "agent", "runner": runner, "class": "com.dorker.sample.SampleTests#passing"}));
    assert_eq!(t["structuredContent"]["ok"], true);

    // A conflict is a tool error that carries the CLI's message and the exit-code meaning.
    let dup = c.tool("dorker_run", json!({"image": "mcp:e2e", "name": "agent"}));
    assert_eq!(dup["isError"], true);
    assert!(
        dup["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("already in use")
    );

    let rm = c.tool("dorker_rm", json!({"instance": "agent"}));
    assert_eq!(rm["structuredContent"]["removed"], json!(["agent"]));
    assert_eq!(
        c.tool("dorker_ps", json!({"all": true}))["structuredContent"]["instances"],
        json!([])
    );
}
