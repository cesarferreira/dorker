//! `dorker mcp`: a Model Context Protocol server on stdio.
//!
//! Each tool runs the `dorker` CLI itself with `--json`, so the server reuses all the CLI's
//! behaviour, exit codes and on-disk state, and a failing tool cannot take the server down.
//! Messages are newline-delimited JSON-RPC 2.0; stdout carries protocol messages only.

use anyhow::Result;
use serde_json::{Value, json};
use std::io::{BufRead, Write};
use std::process::Command;
use std::sync::{Arc, Mutex};

const SUPPORTED_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

const INSTRUCTIONS: &str = "dorker manages disposable headless Android emulators. Typical flow: \
dorker_doctor (check setup), dorker_run (start an instance; android:35 or a built image) → \
dorker_install / dorker_exec / dorker_screenshot / dorker_logs / dorker_test → dorker_rm (always \
remove instances you started). Starting an instance takes a few seconds from a snapshot-backed \
image and about 15-60 seconds cold. Instances persist between calls and are addressed by name.";

/// What a tool call produced: MCP content blocks, an optional structured object, and an error flag.
pub struct ToolResult {
    pub content: Vec<Value>,
    pub structured: Option<Value>,
    pub is_error: bool,
}

impl ToolResult {
    pub fn text(s: impl Into<String>, is_error: bool) -> Self {
        ToolResult {
            content: vec![json!({"type": "text", "text": s.into()})],
            structured: None,
            is_error,
        }
    }

    pub fn json(v: Value) -> Self {
        let text = serde_json::to_string_pretty(&v).unwrap_or_default();
        ToolResult {
            content: vec![json!({"type": "text", "text": text})],
            structured: Some(v),
            is_error: false,
        }
    }
}

// ---------- tool catalogue ----------

fn schema(props: Value, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": props, "required": required, "additionalProperties": false })
}

fn tool(
    name: &str,
    title: &str,
    description: &str,
    input: Value,
    read_only: bool,
    destructive: bool,
) -> Value {
    json!({
        "name": name,
        "title": title,
        "description": description,
        "inputSchema": input,
        "annotations": { "readOnlyHint": read_only, "destructiveHint": destructive, "openWorldHint": false },
    })
}

pub fn tools() -> Vec<Value> {
    let instance = json!({"type": "string", "description": "Instance name or id (from dorker_run / dorker_ps)"});
    vec![
        tool(
            "dorker_doctor",
            "Check setup",
            "Check the host: SDK, emulator, adb, acceleration, disk, installed system images. Returns ok plus per-check status and fixes.",
            schema(json!({}), &[]),
            true,
            false,
        ),
        tool(
            "dorker_ps",
            "List instances",
            "List dorker instances with status and adb serial.",
            schema(
                json!({"all": {"type": "boolean", "description": "Include stopped instances"}}),
                &[],
            ),
            true,
            false,
        ),
        tool(
            "dorker_images",
            "List images",
            "List built dorker images.",
            schema(json!({}), &[]),
            true,
            false,
        ),
        tool(
            "dorker_build",
            "Build an image",
            "Build an immutable image from a Dorkerfile (installs APKs, applies settings, saves a snapshot). Slow the first time (about a minute); instant if nothing changed.",
            schema(
                json!({
                    "context": {"type": "string", "description": "Build context directory containing the Dorkerfile (default \".\")"},
                    "tag": {"type": "string", "description": "Image name, e.g. myapp:latest"},
                    "file": {"type": "string", "description": "Dorkerfile path if not <context>/Dorkerfile"},
                    "no_cache": {"type": "boolean"}
                }),
                &[],
            ),
            false,
            false,
        ),
        tool(
            "dorker_run",
            "Start an instance",
            "Start a headless Android instance from `android:<api>` (e.g. android:35) or a built image, wait until it is ready, and return its details. Remove it with dorker_rm when done.",
            schema(
                json!({
                    "image": {"type": "string", "description": "android:35 or a built image such as myapp:latest"},
                    "name": {"type": "string", "description": "Instance name (default: generated)"},
                    "memory": {"type": "string", "description": "Guest RAM, e.g. 2GB (overriding an image's value disables its snapshot)"},
                    "cpus": {"type": "integer", "minimum": 1},
                    "rm": {"type": "boolean", "description": "Remove automatically when stopped"}
                }),
                &["image"],
            ),
            false,
            false,
        ),
        tool(
            "dorker_start",
            "Restart an instance",
            "Restart a stopped instance (cold boot; keeps its data).",
            schema(json!({"instance": instance}), &["instance"]),
            false,
            false,
        ),
        tool(
            "dorker_stop",
            "Stop an instance",
            "Stop a running instance (its data is kept).",
            schema(json!({"instance": instance}), &["instance"]),
            false,
            true,
        ),
        tool(
            "dorker_rm",
            "Remove an instance",
            "Remove an instance and its data, stopping it first if needed.",
            schema(json!({"instance": instance}), &["instance"]),
            false,
            true,
        ),
        tool(
            "dorker_exec",
            "Run an Android shell command",
            "Run a shell command inside the Android guest (e.g. `pm list packages`, `input tap 400 600`, `am start -n pkg/.Activity`). Returns exit_code, stdout and stderr.",
            schema(
                json!({"instance": instance, "command": {"type": "string", "description": "Command line run by the guest's shell"}}),
                &["instance", "command"],
            ),
            false,
            false,
        ),
        tool(
            "dorker_install",
            "Install an APK",
            "Install an APK from a path on this machine into the instance.",
            schema(
                json!({"instance": instance, "apk_path": {"type": "string"}}),
                &["instance", "apk_path"],
            ),
            false,
            false,
        ),
        tool(
            "dorker_screenshot",
            "Take a screenshot",
            "Capture the instance's screen as a PNG image.",
            schema(json!({"instance": instance}), &["instance"]),
            true,
            false,
        ),
        tool(
            "dorker_logs",
            "Read logcat",
            "Return the last N lines of the Android log.",
            schema(
                json!({"instance": instance, "lines": {"type": "integer", "minimum": 1, "description": "Number of lines (default 200)"}}),
                &["instance"],
            ),
            true,
            false,
        ),
        tool(
            "dorker_test",
            "Run instrumentation tests",
            "Run an instrumentation runner and return structured results (passed/failed counts, failing tests with stack traces, log directory).",
            schema(
                json!({
                    "instance": instance,
                    "runner": {"type": "string", "description": "e.g. com.example.test/androidx.test.runner.AndroidJUnitRunner"},
                    "class": {"type": "string", "description": "Limit to a class or class#method"},
                    "timeout_secs": {"type": "integer", "minimum": 1}
                }),
                &["instance", "runner"],
            ),
            false,
            false,
        ),
    ]
}

// ---------- argv construction (pure, unit-tested) ----------

/// How a tool's CLI output is turned into a result.
#[derive(Debug, PartialEq, Eq)]
pub enum Kind {
    /// stdout is JSON; wrap arrays under this key when given.
    Json(Option<&'static str>),
    /// stdout is an image id; return {image_id, tag}.
    Build,
    /// Return {"<key>": <stdout trimmed>} (stop/rm print names).
    Names(&'static str),
    /// Plain text stdout (logs).
    Text,
    /// Screenshot: the PNG arrives as raw bytes on stdout (`screenshot <i> -`), no temp file.
    Screenshot,
}

#[derive(Debug)]
pub struct Invocation {
    pub argv: Vec<String>,
    pub kind: Kind,
}

fn req_str(args: &Value, key: &str) -> Result<String, String> {
    match args.get(key) {
        Some(Value::String(s)) if !s.is_empty() => Ok(s.clone()),
        Some(Value::String(_)) => Err(format!("`{key}` must not be empty")),
        Some(_) => Err(format!("`{key}` must be a string")),
        None => Err(format!("missing required argument `{key}`")),
    }
}

fn opt_str(args: &Value, key: &str) -> Result<Option<String>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(format!("`{key}` must be a string")),
    }
}

fn opt_bool(args: &Value, key: &str) -> Result<bool, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(format!("`{key}` must be a boolean")),
    }
}

fn opt_uint(args: &Value, key: &str) -> Result<Option<u64>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_u64()
            .filter(|n| *n >= 1)
            .map(Some)
            .ok_or_else(|| format!("`{key}` must be a positive integer")),
    }
}

/// Subcommand names and values never start with `-` for instance/image arguments, so a
/// model-supplied value cannot smuggle in extra flags.
fn no_flag(key: &str, v: &str) -> Result<(), String> {
    if v.starts_with('-') {
        Err(format!("`{key}` must not start with '-'"))
    } else {
        Ok(())
    }
}

pub fn build_invocation(name: &str, args: &Value) -> Result<Invocation, String> {
    if !args.is_object() && !args.is_null() {
        return Err("arguments must be an object".into());
    }
    let s = |v: &str| v.to_string();
    let (argv, kind): (Vec<String>, Kind) = match name {
        "dorker_doctor" => (vec![s("doctor"), s("--json")], Kind::Json(None)),
        "dorker_images" => (vec![s("images"), s("--json")], Kind::Json(Some("images"))),
        "dorker_ps" => {
            let mut v = vec![s("ps"), s("--json")];
            if opt_bool(args, "all")? {
                v.push(s("-a"));
            }
            (v, Kind::Json(Some("instances")))
        }
        "dorker_build" => {
            let context = opt_str(args, "context")?.unwrap_or_else(|| ".".into());
            let mut v = vec![s("build")];
            if let Some(t) = opt_str(args, "tag")? {
                no_flag("tag", &t)?;
                v.extend([s("-t"), t]);
            }
            if let Some(f) = opt_str(args, "file")? {
                v.extend([s("-f"), f]);
            }
            if opt_bool(args, "no_cache")? {
                v.push(s("--no-cache"));
            }
            v.push(s("--"));
            v.push(context);
            (v, Kind::Build)
        }
        "dorker_run" => {
            let image = req_str(args, "image")?;
            no_flag("image", &image)?;
            let mut v = vec![s("run"), s("-d"), s("--json")];
            if let Some(n) = opt_str(args, "name")? {
                no_flag("name", &n)?;
                v.extend([s("--name"), n]);
            }
            if let Some(m) = opt_str(args, "memory")? {
                no_flag("memory", &m)?;
                v.extend([s("--memory"), m]);
            }
            if let Some(c) = opt_uint(args, "cpus")? {
                v.extend([s("--cpus"), c.to_string()]);
            }
            if opt_bool(args, "rm")? {
                v.push(s("--rm"));
            }
            v.push(image);
            (v, Kind::Json(None))
        }
        "dorker_start" => {
            let i = req_str(args, "instance")?;
            no_flag("instance", &i)?;
            (vec![s("start"), s("--json"), i], Kind::Json(None))
        }
        "dorker_stop" => {
            let i = req_str(args, "instance")?;
            no_flag("instance", &i)?;
            (vec![s("stop"), i], Kind::Names("stopped"))
        }
        "dorker_rm" => {
            let i = req_str(args, "instance")?;
            no_flag("instance", &i)?;
            (vec![s("rm"), s("-f"), i], Kind::Names("removed"))
        }
        "dorker_exec" => {
            let i = req_str(args, "instance")?;
            no_flag("instance", &i)?;
            let c = req_str(args, "command")?;
            // The whole command line is ONE argument (the guest's shell parses it).
            (vec![s("exec"), s("--json"), i, c], Kind::Json(None))
        }
        "dorker_install" => {
            let i = req_str(args, "instance")?;
            no_flag("instance", &i)?;
            let apk = req_str(args, "apk_path")?;
            (vec![s("install"), i, apk, s("--json")], Kind::Json(None))
        }
        "dorker_screenshot" => {
            let i = req_str(args, "instance")?;
            no_flag("instance", &i)?;
            (vec![s("screenshot"), i, s("-")], Kind::Screenshot)
        }
        "dorker_logs" => {
            let i = req_str(args, "instance")?;
            no_flag("instance", &i)?;
            let n = opt_uint(args, "lines")?.unwrap_or(200);
            (vec![s("logs"), i, s("--tail"), n.to_string()], Kind::Text)
        }
        "dorker_test" => {
            let i = req_str(args, "instance")?;
            no_flag("instance", &i)?;
            let runner = req_str(args, "runner")?;
            let mut v = vec![s("test"), i, s("--runner"), runner, s("--json")];
            if let Some(c) = opt_str(args, "class")? {
                v.extend([s("--class"), c]);
            }
            if let Some(t) = opt_uint(args, "timeout_secs")? {
                v.extend([s("--timeout"), t.to_string()]);
            }
            (v, Kind::Json(None))
        }
        other => return Err(format!("unknown tool {other:?}")),
    };
    Ok(Invocation { argv, kind })
}

// ---------- base64 (no dependency needed for one use) ----------

pub fn base64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

// ---------- executing a tool ----------

/// Meaning of dorker's stable exit codes, for error messages.
fn exit_meaning(code: i32) -> &'static str {
    match code {
        2 => "invalid configuration or arguments",
        3 => "missing dependency",
        4 => "build failed",
        5 => "instance failed to start",
        6 => "adb operation failed",
        7 => "tests failed",
        8 => "conflict or not enough disk space",
        _ => "error",
    }
}

fn tail_chars(s: &str, max: usize) -> String {
    let t = s.trim();
    if t.chars().count() <= max {
        t.to_string()
    } else {
        let skip = t.chars().count() - max;
        format!("...{}", t.chars().skip(skip).collect::<String>())
    }
}

/// Run `dorker <argv>` and interpret its output per `kind`.
pub fn run_cli(exe: &std::path::Path, inv: &Invocation) -> ToolResult {
    let out = match Command::new(exe).args(&inv.argv).output() {
        Ok(o) => o,
        Err(e) => return ToolResult::text(format!("could not run dorker: {e}"), true),
    };
    let code = out.status.code().unwrap_or(1);
    // A screenshot's stdout is binary PNG data, so only decode text results.
    let stdout = if inv.kind == Kind::Screenshot {
        String::new()
    } else {
        String::from_utf8_lossy(&out.stdout).to_string()
    };
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let failure = |code: i32| {
        let detail = if stderr.trim().is_empty() {
            stdout.as_str()
        } else {
            stderr.as_str()
        };
        ToolResult::text(
            format!(
                "{} (exit code {code}: {})",
                tail_chars(detail, 4000),
                exit_meaning(code)
            ),
            true,
        )
    };

    match &inv.kind {
        Kind::Json(wrap) => match serde_json::from_str::<Value>(&stdout) {
            // A parseable result is returned even on a non-zero exit (exec, test and doctor
            // report failures inside the JSON).
            Ok(v) => ToolResult::json(match (wrap, v) {
                (Some(key), v) if !v.is_object() => json!({ *key: v }),
                (_, v) => v,
            }),
            Err(_) => failure(code),
        },
        Kind::Build if code == 0 => ToolResult::json(
            json!({"image_id": stdout.trim(), "build_log": tail_chars(&stderr, 2000)}),
        ),
        Kind::Names(key) if code == 0 => ToolResult::json(
            json!({ *key: stdout.lines().map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>() }),
        ),
        Kind::Text if code == 0 => ToolResult::text(
            if stdout.trim().is_empty() {
                "(no log output)".to_string()
            } else {
                stdout
            },
            false,
        ),
        Kind::Screenshot if code == 0 => {
            let png = &out.stdout;
            if png.starts_with(b"\x89PNG\r\n\x1a\n") {
                ToolResult {
                    content: vec![
                        json!({"type": "image", "data": base64_encode(png), "mimeType": "image/png"}),
                        json!({"type": "text", "text": format!("screenshot, {} bytes", png.len())}),
                    ],
                    structured: None,
                    is_error: false,
                }
            } else {
                ToolResult::text("screenshot did not return PNG data", true)
            }
        }
        _ => failure(code),
    }
}

// ---------- JSON-RPC handling (pure, unit-tested) ----------

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn result_value(r: &ToolResult) -> Value {
    let mut v = json!({"content": r.content, "isError": r.is_error});
    if let Some(s) = &r.structured {
        v["structuredContent"] = s.clone();
    }
    v
}

/// Handle one JSON-RPC message. Returns the response, or `None` for notifications.
/// `call` executes a tool by name.
pub fn handle(msg: &Value, call: &dyn Fn(&str, &Value) -> ToolResult) -> Option<Value> {
    let Some(method) = msg.get("method").and_then(Value::as_str) else {
        // A response or malformed message from the client: nothing to answer.
        return msg
            .get("id")
            .map(|id| rpc_error(id.clone(), -32600, "invalid request: missing method"));
    };
    let id = msg.get("id").cloned();
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let Some(id) = id else {
        return None; // notification (e.g. notifications/initialized, notifications/cancelled)
    };
    let ok = |result: Value| json!({"jsonrpc": "2.0", "id": id, "result": result});
    Some(match method {
        "initialize" => {
            let requested = params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or("");
            let version = if SUPPORTED_VERSIONS.contains(&requested) {
                requested
            } else {
                SUPPORTED_VERSIONS[0]
            };
            ok(json!({
                "protocolVersion": version,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": "dorker", "title": "dorker", "version": env!("CARGO_PKG_VERSION") },
                "instructions": INSTRUCTIONS,
            }))
        }
        "ping" => ok(json!({})),
        "tools/list" => ok(json!({ "tools": tools() })),
        "tools/call" => {
            let Some(name) = params.get("name").and_then(Value::as_str) else {
                return Some(rpc_error(id, -32602, "tools/call needs a tool name"));
            };
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            ok(result_value(&call(name, &args)))
        }
        other => rpc_error(id, -32601, &format!("method not found: {other}")),
    })
}

/// Execute a tool via the CLI; invalid arguments become an `isError` result so the model
/// can correct itself.
pub fn execute(exe: &std::path::Path, name: &str, args: &Value) -> ToolResult {
    match build_invocation(name, args) {
        Ok(inv) => run_cli(exe, &inv),
        Err(msg) => ToolResult::text(format!("invalid arguments for {name}: {msg}"), true),
    }
}

/// Serve MCP on stdin/stdout until stdin closes. Tool calls run on their own threads so a slow
/// build does not block other requests.
pub fn serve() -> Result<i32> {
    let exe = std::env::current_exe()?;
    let stdout = Arc::new(Mutex::new(std::io::stdout()));
    let send = {
        let stdout = Arc::clone(&stdout);
        move |v: &Value| {
            if let Ok(mut out) = stdout.lock() {
                let _ = writeln!(out, "{v}");
                let _ = out.flush();
            }
        }
    };
    crate::log::vlog!("mcp server ready (dorker {})", env!("CARGO_PKG_VERSION"));
    let mut workers = Vec::new();
    for line in std::io::stdin().lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                send(&rpc_error(
                    Value::Null,
                    -32700,
                    &format!("parse error: {e}"),
                ));
                continue;
            }
        };
        let is_call = msg.get("method").and_then(Value::as_str) == Some("tools/call")
            && msg.get("id").is_some();
        if is_call {
            let exe = exe.clone();
            let send = send.clone();
            workers.push(std::thread::spawn(move || {
                if let Some(resp) = handle(&msg, &|n, a| execute(&exe, n, a)) {
                    send(&resp);
                }
            }));
            workers.retain(|w| !w.is_finished());
        } else if let Some(resp) = handle(&msg, &|n, a| execute(&exe, n, a)) {
            send(&resp);
        }
    }
    for w in workers {
        let _ = w.join(); // answer in-flight calls before exiting
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake(name: &str, args: &Value) -> ToolResult {
        ToolResult::json(json!({"called": name, "args": args}))
    }

    fn rpc(method: &str, params: Value) -> Value {
        json!({"jsonrpc": "2.0", "id": 7, "method": method, "params": params})
    }

    #[test]
    fn initialize_negotiates_version_and_declares_tools() {
        let r = handle(&rpc("initialize", json!({"protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}})), &fake).unwrap();
        assert_eq!(r["id"], 7);
        assert_eq!(r["result"]["protocolVersion"], "2025-03-26");
        assert!(r["result"]["capabilities"]["tools"].is_object());
        assert_eq!(r["result"]["serverInfo"]["name"], "dorker");
        assert!(
            r["result"]["instructions"]
                .as_str()
                .unwrap()
                .contains("dorker_rm")
        );
        // Unknown versions get the latest supported one.
        let r = handle(
            &rpc("initialize", json!({"protocolVersion": "1999-01-01"})),
            &fake,
        )
        .unwrap();
        assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
    }

    #[test]
    fn notifications_get_no_response_and_unknown_methods_error() {
        assert!(
            handle(
                &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
                &fake
            )
            .is_none()
        );
        let r = handle(&rpc("nope/nothing", Value::Null), &fake).unwrap();
        assert_eq!(r["error"]["code"], -32601);
        let r = handle(&rpc("ping", Value::Null), &fake).unwrap();
        assert_eq!(r["result"], json!({}));
        let r = handle(&json!({"jsonrpc": "2.0", "id": 1}), &fake).unwrap();
        assert_eq!(r["error"]["code"], -32600);
    }

    #[test]
    fn tools_list_is_well_formed() {
        let r = handle(&rpc("tools/list", Value::Null), &fake).unwrap();
        let tools = r["result"]["tools"].as_array().unwrap();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        for want in [
            "dorker_doctor",
            "dorker_run",
            "dorker_exec",
            "dorker_screenshot",
            "dorker_test",
            "dorker_rm",
            "dorker_ps",
            "dorker_build",
            "dorker_logs",
            "dorker_install",
            "dorker_start",
            "dorker_stop",
            "dorker_images",
        ] {
            assert!(names.contains(&want), "missing {want}");
        }
        for t in tools {
            assert_eq!(t["inputSchema"]["type"], "object", "{}", t["name"]);
            assert!(t["description"].as_str().unwrap().len() > 10);
            // Every required property must be declared.
            for req in t["inputSchema"]["required"].as_array().unwrap() {
                assert!(
                    t["inputSchema"]["properties"]
                        .get(req.as_str().unwrap())
                        .is_some(),
                    "{} requires undeclared {req}",
                    t["name"]
                );
            }
        }
        let mut sorted = names.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len(), "duplicate tool names");
    }

    #[test]
    fn tools_call_returns_content_and_structured_result() {
        let r = handle(
            &rpc(
                "tools/call",
                json!({"name": "dorker_ps", "arguments": {"all": true}}),
            ),
            &fake,
        )
        .unwrap();
        assert_eq!(r["result"]["isError"], false);
        assert_eq!(r["result"]["structuredContent"]["called"], "dorker_ps");
        assert_eq!(r["result"]["content"][0]["type"], "text");
        let r = handle(&rpc("tools/call", json!({"arguments": {}})), &fake).unwrap();
        assert_eq!(r["error"]["code"], -32602);
    }

    #[test]
    fn argv_is_built_without_a_shell_and_flags_cannot_be_smuggled() {
        let inv = build_invocation(
            "dorker_exec",
            &json!({"instance": "t1", "command": "pm list packages | grep x; rm -rf /"}),
        )
        .unwrap();
        assert_eq!(
            inv.argv,
            [
                "exec",
                "--json",
                "t1",
                "pm list packages | grep x; rm -rf /"
            ],
            "the command stays ONE argument"
        );
        assert_eq!(inv.kind, Kind::Json(None));
        let inv = build_invocation(
            "dorker_run",
            &json!({"image": "android:35", "name": "a", "cpus": 2, "memory": "2GB", "rm": true}),
        )
        .unwrap();
        assert_eq!(
            inv.argv,
            [
                "run",
                "-d",
                "--json",
                "--name",
                "a",
                "--memory",
                "2GB",
                "--cpus",
                "2",
                "--rm",
                "android:35"
            ]
        );
        // A model-supplied value must not become a flag.
        for (tool, args) in [
            ("dorker_run", json!({"image": "--help"})),
            (
                "dorker_exec",
                json!({"instance": "--json", "command": "ls"}),
            ),
            ("dorker_rm", json!({"instance": "-a"})),
            ("dorker_build", json!({"tag": "-x"})),
        ] {
            let e = build_invocation(tool, &args).unwrap_err();
            assert!(e.contains("must not start with '-'"), "{tool}: {e}");
        }
        // The build context goes after `--` so it cannot be read as a flag either.
        let inv = build_invocation(
            "dorker_build",
            &json!({"context": "--evil", "tag": "app:1"}),
        )
        .unwrap();
        assert_eq!(inv.argv, ["build", "-t", "app:1", "--", "--evil"]);
    }

    #[test]
    fn argument_validation_messages_name_the_problem() {
        assert!(
            build_invocation("dorker_run", &json!({}))
                .unwrap_err()
                .contains("missing required argument `image`")
        );
        assert!(
            build_invocation("dorker_run", &json!({"image": 5}))
                .unwrap_err()
                .contains("must be a string")
        );
        assert!(
            build_invocation("dorker_run", &json!({"image": "android:35", "cpus": 0}))
                .unwrap_err()
                .contains("positive integer")
        );
        assert!(
            build_invocation("dorker_ps", &json!({"all": "yes"}))
                .unwrap_err()
                .contains("boolean")
        );
        assert!(
            build_invocation("dorker_nope", &json!({}))
                .unwrap_err()
                .contains("unknown tool")
        );
        assert!(
            build_invocation("dorker_ps", &json!(["x"]))
                .unwrap_err()
                .contains("object")
        );
        assert_eq!(
            build_invocation("dorker_logs", &json!({"instance": "t"}))
                .unwrap()
                .argv,
            ["logs", "t", "--tail", "200"]
        );
        assert_eq!(
            build_invocation(
                "dorker_test",
                &json!({"instance": "t", "runner": "a/b", "class": "C#m", "timeout_secs": 30})
            )
            .unwrap()
            .argv,
            [
                "test",
                "t",
                "--runner",
                "a/b",
                "--json",
                "--class",
                "C#m",
                "--timeout",
                "30"
            ]
        );
    }

    #[test]
    fn screenshot_never_uses_a_temp_file() {
        let inv = build_invocation("dorker_screenshot", &json!({"instance": "t1"})).unwrap();
        // The PNG comes back on stdout (`-`); there is no path to predict or race on.
        assert_eq!(inv.argv, ["screenshot", "t1", "-"]);
        assert_eq!(inv.kind, Kind::Screenshot);
        assert!(
            !include_str!("mcp.rs").contains(concat!("temp", "_dir()")),
            "mcp.rs must not create temp files"
        );
    }

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_encode(&[0x89, b'P', b'N', b'G']), "iVBORw==");
    }

    #[test]
    fn invalid_arguments_become_a_tool_error_not_a_protocol_error() {
        let r = execute(
            std::path::Path::new("/nonexistent/dorker"),
            "dorker_run",
            &json!({}),
        );
        assert!(r.is_error);
        assert!(
            r.content[0]["text"]
                .as_str()
                .unwrap()
                .contains("invalid arguments for dorker_run")
        );
        let r = execute(
            std::path::Path::new("/nonexistent/dorker"),
            "dorker_ps",
            &json!({}),
        );
        assert!(r.is_error, "a missing executable is reported, not a panic");
    }

    #[test]
    fn tail_keeps_the_end_of_long_output() {
        assert_eq!(tail_chars("  short  ", 100), "short");
        let long = "x".repeat(50) + "END";
        assert_eq!(tail_chars(&long, 10), "...xxxxxxxEND");
    }
}
