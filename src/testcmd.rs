//! `dorker test`: run an instrumentation runner and report a verdict.

use crate::errors;
use crate::runtime;
use crate::sdk::Sdk;
use crate::store::{State, Store, now_secs};
use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::io::{BufRead, Read};
use std::process::Stdio;
use std::time::{Duration, Instant};

#[derive(Debug, Default, PartialEq)]
pub struct Report {
    pub passed: u32,
    pub failed: u32,
    pub errors: u32,
    pub ignored: u32,
    pub failures: Vec<Failure>,
    /// Every test that reported a result, in order.
    pub tests: Vec<TestCase>,
    pub final_code: Option<i32>,
    pub crashed: bool,
    pub failed_to_start: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub class: String,
    pub test: String,
    pub stack: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestStatus {
    Passed,
    /// An assertion failed (status -2).
    Failed,
    /// The test threw something else (status -1).
    Error,
    /// `@Ignore` (-3) or a failed assumption (-4).
    Skipped,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TestCase {
    pub class: String,
    pub test: String,
    pub status: TestStatus,
    /// Seconds between the test's start and end status lines, as they arrived on the host.
    /// None when the output was not timed or the start line is missing.
    pub secs: Option<f64>,
}

impl TestCase {
    pub fn id(&self) -> String {
        format!("{}#{}", self.class, self.test)
    }
}

impl TestStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            TestStatus::Passed => "passed",
            TestStatus::Failed => "failed",
            TestStatus::Error => "error",
            TestStatus::Skipped => "skipped",
        }
    }
}

impl Report {
    pub fn ok(&self) -> bool {
        self.failed_to_start.is_none()
            && !self.crashed
            && self.failed == 0
            && self.errors == 0
            && self.final_code == Some(-1)
    }
}

/// Parse `am instrument -r` (raw) output.
#[cfg(test)]
pub fn parse(output: &str) -> Report {
    parse_lines(output.lines().map(|l| (None, l)))
}

/// Parse output whose lines carry the time they arrived (seconds since the run started), so
/// each test gets a measured duration.
pub fn parse_timed(lines: &[(f64, String)]) -> Report {
    parse_lines(lines.iter().map(|(t, l)| (Some(*t), l.as_str())))
}

fn parse_lines<'a>(lines: impl Iterator<Item = (Option<f64>, &'a str)>) -> Report {
    let mut r = Report::default();
    let mut block: HashMap<String, String> = HashMap::new();
    let mut last_key: Option<String> = None;
    let mut started: HashMap<String, f64> = HashMap::new();
    for (at, line) in lines {
        let line = line.trim_end_matches(['\r', '\n']);
        if let Some(rest) = line.strip_prefix("INSTRUMENTATION_STATUS_CODE:") {
            let code: i32 = rest.trim().parse().unwrap_or(i32::MIN);
            let get = |k: &str| block.get(k).cloned().unwrap_or_default();
            let key = format!("{}#{}", get("class"), get("test"));
            if let Some(err) = block.get("Error") {
                // The activity manager reports "could not start" as a status block.
                r.failed_to_start = Some(err.clone()); // more specific than the onError line
                block.clear();
                last_key = None;
                continue;
            }
            let status = match code {
                0 => {
                    r.passed += 1;
                    Some(TestStatus::Passed)
                }
                -1 => {
                    r.errors += 1;
                    r.failures.push(Failure {
                        class: get("class"),
                        test: get("test"),
                        stack: get("stack"),
                    });
                    Some(TestStatus::Error)
                }
                -2 => {
                    r.failed += 1;
                    r.failures.push(Failure {
                        class: get("class"),
                        test: get("test"),
                        stack: get("stack"),
                    });
                    Some(TestStatus::Failed)
                }
                -3 | -4 => {
                    r.ignored += 1;
                    Some(TestStatus::Skipped)
                }
                1 => {
                    if let Some(t) = at {
                        started.insert(key.clone(), t);
                    }
                    None
                }
                _ => None,
            };
            if let Some(status) = status {
                let secs = match (started.remove(&key), at) {
                    (Some(s), Some(e)) => Some((e - s).max(0.0)),
                    _ => None,
                };
                r.tests.push(TestCase {
                    class: get("class"),
                    test: get("test"),
                    status,
                    secs,
                });
            }
            block.clear();
            last_key = None;
        } else if let Some(rest) = line.strip_prefix("INSTRUMENTATION_STATUS:") {
            if let Some((k, v)) = rest.trim_start().split_once('=') {
                block.insert(k.to_string(), v.to_string());
                last_key = Some(k.to_string());
            }
        } else if let Some(rest) = line.strip_prefix("INSTRUMENTATION_CODE:") {
            r.final_code = rest.trim().parse().ok();
            last_key = None;
        } else if line.starts_with("INSTRUMENTATION_RESULT:") {
            if line.contains("shortMsg=Process crashed") {
                r.crashed = true;
            }
            last_key = None;
        } else if line.contains("INSTRUMENTATION_FAILED") && r.failed_to_start.is_none() {
            r.failed_to_start = Some(line.trim().to_string());
        } else if let Some(k) = &last_key {
            // Continuation line of a multi-line value (stack traces).
            if let Some(v) = block.get_mut(k) {
                v.push('\n');
                v.push_str(line);
            }
        }
    }
    r
}

fn safe(s: &str, extra: &str) -> bool {
    !s.is_empty()
        && s.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '/' | '-') || extra.contains(c)
        })
}

/// A test run as JSON, for `dorker test --json` and the MCP server.
pub fn report_json(r: &Report, timed_out: bool, logdir: &std::path::Path) -> serde_json::Value {
    serde_json::json!({
        "ok": r.ok() && !timed_out,
        "passed": r.passed,
        "failed": r.failed,
        "errors": r.errors,
        "ignored": r.ignored,
        "failures": r.failures.iter().map(|f| serde_json::json!({
            "class": f.class, "test": f.test, "stack": f.stack,
        })).collect::<Vec<_>>(),
        "timed_out": timed_out,
        "crashed": r.crashed,
        "failed_to_start": r.failed_to_start,
        "log_dir": logdir,
    })
}

/// Check `--runner` and `--class` before anything is started; they end up in a device shell.
pub fn validate(runner: &str, class: Option<&str>) -> Result<()> {
    let cfg = errors::with(errors::CONFIG);
    if !safe(runner, "") || !runner.contains('/') {
        return Err(cfg(anyhow::anyhow!(
            "invalid --runner {runner:?}: expected <package>/<InstrumentationClass>"
        )));
    }
    if let Some(c) = class {
        if !safe(c, "#,") {
            return Err(cfg(anyhow::anyhow!("invalid --class {c:?}")));
        }
    }
    Ok(())
}

/// One of `count` fixed partitions of a suite, in AndroidJUnitRunner's terms
/// (`-e numShards <count> -e shardIndex <index>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shard {
    pub count: u32,
    pub index: u32,
}

impl Shard {
    pub fn new(count: u32, index: u32) -> Result<Self> {
        if count == 0 || index >= count {
            return Err(errors::with(errors::CONFIG)(anyhow::anyhow!(
                "invalid shard {index} of {count}: the index must be 0..{count} and the count at least 1"
            )));
        }
        Ok(Shard { count, index })
    }
}

/// What to run on one device.
pub struct Options<'a> {
    pub runner: &'a str,
    pub class: Option<&'a str>,
    /// Seconds the instrumentation may run.
    pub timeout: u64,
    pub shard: Option<Shard>,
}

/// The `am instrument` command line. Arguments are validated, so it is shell-safe.
pub fn instrument_command(o: &Options) -> String {
    let mut remote = String::from("am instrument -w -r");
    if let Some(s) = o.shard {
        remote.push_str(&format!(
            " -e numShards {} -e shardIndex {}",
            s.count, s.index
        ));
    }
    if let Some(c) = o.class {
        remote.push_str(&format!(" -e class {c}"));
    }
    remote.push(' ');
    remote.push_str(o.runner);
    remote
}

/// How one instrumentation run ended, most specific cause first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Finished, and every test passed (or there were none).
    Passed,
    /// Finished with failing or erroring tests.
    Failed,
    /// The instrumentation process crashed, or finished with an unexpected code.
    RunnerError,
    /// The runner could not be started (not installed, bad arguments).
    NotStarted,
    /// The output stopped before the runner's final result (adb or the device went away).
    Incomplete,
    TimedOut,
    Cancelled,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Passed => "passed",
            Status::Failed => "failed",
            Status::RunnerError => "runner_error",
            Status::NotStarted => "not_started",
            Status::Incomplete => "incomplete",
            Status::TimedOut => "timed_out",
            Status::Cancelled => "cancelled",
        }
    }

    pub fn of(r: &Report, timed_out: bool, cancelled: bool) -> Status {
        if cancelled {
            Status::Cancelled
        } else if timed_out {
            Status::TimedOut
        } else if r.failed_to_start.is_some() {
            Status::NotStarted
        } else if r.crashed {
            Status::RunnerError
        } else {
            match r.final_code {
                None => Status::Incomplete,
                Some(-1) if r.failed == 0 && r.errors == 0 => Status::Passed,
                Some(-1) => Status::Failed,
                Some(_) => Status::RunnerError,
            }
        }
    }
}

/// The result of running instrumentation on one device.
pub struct Execution {
    pub report: Report,
    pub status: Status,
    /// Seconds the instrumentation ran (not counting log collection).
    pub secs: f64,
    /// Diagnostics that could not be collected, e.g. "logcat.txt: ...". They never change
    /// the status.
    pub collection_errors: Vec<String>,
}

/// How long one diagnostic collection (logcat dump, screenshot) may take.
/// `DORKER_COLLECT_TIMEOUT_SECS` overrides the default of 30 seconds.
pub fn collect_timeout() -> Duration {
    let secs = std::env::var("DORKER_COLLECT_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30);
    Duration::from_secs(secs)
}

/// Run `cmd`, but give up (and kill it) after `limit`. For diagnostics that must not hang.
pub fn output_within(
    cmd: &mut std::process::Command,
    limit: Duration,
) -> Option<std::process::Output> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut pipe = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = pipe.read_to_end(&mut v);
        v
    });
    let start = Instant::now();
    let status = loop {
        if let Ok(Some(s)) = child.try_wait() {
            break Some(s);
        }
        if start.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let stdout = reader.join().unwrap_or_default();
    status.map(|status| std::process::Output {
        status,
        stdout,
        stderr: vec![],
    })
}

/// Run the instrumentation on `serial` and keep its raw output (`instrument.txt`) and the
/// device log (`logcat.txt`) in `logdir`. Prints nothing; the caller presents the result.
/// `cancelled` is polled while it runs; the adb child is killed on cancellation or timeout.
pub fn execute(
    sdk: &Sdk,
    serial: &str,
    o: &Options,
    logdir: &std::path::Path,
    cancelled: &dyn Fn() -> bool,
) -> Result<Execution> {
    std::fs::create_dir_all(logdir)?;
    let _ = output_within(
        runtime::adb(sdk, serial).args(["logcat", "-c"]),
        Duration::from_secs(10),
    );
    let start = Instant::now();
    let mut child = runtime::adb(sdk, serial)
        .args(["shell", &instrument_command(o)])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("running adb")
        .map_err(errors::with(errors::ADB))?;
    let out_pipe = child.stdout.take().unwrap();
    let mut err_pipe = child.stderr.take().unwrap();
    // Each line is stamped when it arrives, which is what the per-test durations are.
    let out_t = std::thread::spawn(move || {
        let mut lines = Vec::new();
        let mut reader = std::io::BufReader::new(out_pipe);
        let mut buf = Vec::new();
        while matches!(reader.read_until(b'\n', &mut buf), Ok(n) if n > 0) {
            let at = start.elapsed().as_secs_f64();
            lines.push((at, String::from_utf8_lossy(&buf).into_owned()));
            buf.clear();
        }
        lines
    });
    let err_t = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = err_pipe.read_to_string(&mut s);
        s
    });
    let (mut timed_out, mut was_cancelled) = (false, false);
    loop {
        if child.try_wait()?.is_some() {
            break;
        }
        timed_out = start.elapsed() > Duration::from_secs(o.timeout);
        was_cancelled = cancelled();
        if timed_out || was_cancelled {
            let _ = child.kill();
            let _ = child.wait();
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let secs = start.elapsed().as_secs_f64();
    let mut lines = out_t.join().unwrap_or_default();
    let stderr = err_t.join().unwrap_or_default();
    let stdout: String = lines.iter().map(|(_, l)| l.as_str()).collect();
    std::fs::write(logdir.join("instrument.txt"), format!("{stdout}{stderr}"))?;
    let mut collection_errors = Vec::new();
    let limit = collect_timeout();
    match output_within(runtime::adb(sdk, serial).args(["logcat", "-d"]), limit) {
        Some(o) => {
            if let Err(e) = std::fs::write(logdir.join("logcat.txt"), o.stdout) {
                collection_errors.push(format!("logcat.txt: {e}"));
            }
        }
        None => collection_errors.push(format!(
            "logcat.txt: adb logcat did not finish within {}s",
            limit.as_secs()
        )),
    }
    // Some failures (e.g. "target has no code") arrive on stderr; parse both.
    lines.extend(stderr.lines().map(|l| (secs, l.to_string())));
    let report = parse_timed(&lines);
    let status = Status::of(&report, timed_out, was_cancelled);
    Ok(Execution {
        report,
        status,
        secs,
        collection_errors,
    })
}

/// Explain why a runner could not start: list the instrumentation that is installed.
pub fn explain_not_started(sdk: &Sdk, serial: &str) -> String {
    let listed = output_within(
        runtime::adb(sdk, serial).args(["shell", "pm", "list", "instrumentation"]),
        Duration::from_secs(15),
    )
    .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    .unwrap_or_default();
    if listed.is_empty() {
        "No instrumentation is installed in this instance. Install a test APK with `dorker install`."
            .to_string()
    } else {
        let mut s = String::from("Instrumentation installed in this instance:");
        for l in listed.lines() {
            s.push_str("\n  ");
            s.push_str(l.trim_start_matches("instrumentation:"));
        }
        s
    }
}

pub fn run(
    key: &str,
    runner: &str,
    class: Option<&str>,
    timeout: u64,
    json: bool,
    out: Option<&std::path::Path>,
) -> Result<i32> {
    validate(runner, class)?;
    let sdk = Sdk::locate().map_err(errors::with(errors::DEPENDENCY))?;
    let store = Store::open()?;
    let mut inst = store.find(key)?;
    runtime::reconcile(&store, &mut inst)?;
    if inst.state != State::Ready {
        bail!(
            "instance {} is {}, not READY\n\nRun `dorker ps -a` to inspect it.",
            inst.name,
            inst.state.as_str()
        );
    }
    let serial = inst.serial();
    // Two runs in the same second must not share (and overwrite) one log folder.
    let logdir = store.instance_dir(&inst.id).join(format!(
        "test-{}-{}",
        now_secs(),
        &crate::store::new_id()[..6]
    ));
    let exec = execute(
        &sdk,
        &serial,
        &Options {
            runner,
            class,
            timeout,
            shard: None,
        },
        &logdir,
        &runtime::interrupted,
    )?;
    // The single-instance contract reports an interrupted run as timed out.
    let timed_out = matches!(exec.status, Status::TimedOut | Status::Cancelled);
    // The logs live inside the instance's folder, so `dorker rm` deletes them. `--out` keeps a
    // copy somewhere that outlives the instance (for CI artifacts).
    let mut logdir_shown = logdir.clone();
    if let Some(dest) = out {
        std::fs::create_dir_all(dest)
            .map_err(|e| anyhow::anyhow!("creating --out {}: {e}", dest.display()))?;
        for name in ["instrument.txt", "logcat.txt"] {
            if logdir.join(name).is_file() {
                std::fs::copy(logdir.join(name), dest.join(name))
                    .map_err(|e| anyhow::anyhow!("copying {name} to {}: {e}", dest.display()))?;
            }
        }
        logdir_shown = dest.to_path_buf();
    }
    let logdir = logdir_shown;

    let report = exec.report;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report_json(&report, timed_out, &logdir))?
        );
    } else {
        println!(
            "Tests: {} passed, {} failed, {} errors, {} ignored",
            report.passed, report.failed, report.errors, report.ignored
        );
        for f in &report.failures {
            println!("\nFAILED {}#{}\n{}", f.class, f.test, f.stack.trim_end());
        }
    }
    if timed_out {
        eprintln!("error: instrumentation did not finish within {timeout}s");
    }
    if let Some(m) = &report.failed_to_start {
        eprintln!("error: {m}");
        // Android's own detail is not always delivered, so show what can be run.
        eprintln!("{}", explain_not_started(&sdk, &serial));
    }
    if report.crashed {
        eprintln!("error: the instrumentation process crashed");
    }
    if report.ok() && !timed_out {
        if !json {
            println!("\nPASSED (logs: {})", logdir.display());
        }
        Ok(0)
    } else {
        eprintln!("\nFAILED. Logs preserved in {}", logdir.display());
        Ok(errors::TEST)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASS: &str = "INSTRUMENTATION_STATUS: numtests=2\nINSTRUMENTATION_STATUS: class=a.B\nINSTRUMENTATION_STATUS: test=one\nINSTRUMENTATION_STATUS_CODE: 1\n\
INSTRUMENTATION_STATUS: class=a.B\nINSTRUMENTATION_STATUS: test=one\nINSTRUMENTATION_STATUS_CODE: 0\n\
INSTRUMENTATION_STATUS: class=a.B\nINSTRUMENTATION_STATUS: test=two\nINSTRUMENTATION_STATUS_CODE: 0\n\
INSTRUMENTATION_RESULT: stream=\n\nTime: 0.1\n\nOK (2 tests)\n\nINSTRUMENTATION_CODE: -1\n";

    #[test]
    fn passing_run() {
        let r = parse(PASS);
        assert_eq!((r.passed, r.failed, r.errors), (2, 0, 0));
        assert!(r.ok());
    }

    #[test]
    fn failing_test_with_multiline_stack() {
        let out = "INSTRUMENTATION_STATUS: class=a.B\nINSTRUMENTATION_STATUS: test=boom\n\
INSTRUMENTATION_STATUS: stack=java.lang.AssertionError: x\n\tat a.B.boom(B.java:9)\n\tat more\n\
INSTRUMENTATION_STATUS_CODE: -2\nINSTRUMENTATION_CODE: -1\n";
        let r = parse(out);
        assert_eq!(r.failed, 1);
        assert!(!r.ok());
        assert_eq!(r.failures[0].test, "boom");
        assert!(r.failures[0].stack.contains("at a.B.boom"));
        assert!(r.failures[0].stack.starts_with("java.lang.AssertionError"));
    }

    #[test]
    fn crash_start_failure_and_missing_final_code() {
        let r =
            parse("INSTRUMENTATION_RESULT: shortMsg=Process crashed.\nINSTRUMENTATION_CODE: 0\n");
        assert!(r.crashed && !r.ok());
        let r = parse("onError: commandError=true message=INSTRUMENTATION_FAILED: com.nope/X\n");
        assert!(r.failed_to_start.is_some() && !r.ok());
        // No final code (e.g. adb died mid-run) is never a pass.
        assert!(!parse("INSTRUMENTATION_STATUS_CODE: 0\n").ok());
        // Ignored and assumption failures alone do not fail the run.
        let r = parse(
            "INSTRUMENTATION_STATUS_CODE: -3\nINSTRUMENTATION_STATUS_CODE: -4\nINSTRUMENTATION_CODE: -1\n",
        );
        assert_eq!(r.ignored, 2);
        assert!(r.ok());
    }

    #[test]
    fn start_failure_is_not_a_test_error() {
        // Captured from a real emulator (API 35) for a runner that does not exist.
        let out = "onError: commandError=true message=INSTRUMENTATION_FAILED: com.nope/com.nope.X\n\
INSTRUMENTATION_STATUS: Error=Unable to find instrumentation info for: ComponentInfo{com.nope/com.nope.X}\n\
INSTRUMENTATION_STATUS: id=ActivityManagerService\nINSTRUMENTATION_STATUS_CODE: -1\n\
android.util.AndroidException: INSTRUMENTATION_FAILED: com.nope/com.nope.X\n\tat com.android.commands.am.Instrument.run(Instrument.java:543)\n";
        let r = parse(out);
        assert_eq!(r.errors, 0);
        assert!(
            r.failed_to_start
                .as_deref()
                .unwrap()
                .contains("Unable to find instrumentation")
        );
        assert!(!r.ok());
    }

    #[test]
    fn test_identities_are_kept_in_order() {
        let r = parse(PASS);
        let ids: Vec<String> = r.tests.iter().map(TestCase::id).collect();
        assert_eq!(ids, ["a.B#one", "a.B#two"]);
        assert!(r.tests.iter().all(|t| t.status == TestStatus::Passed));
        let r = parse(
            "INSTRUMENTATION_STATUS: class=a.B\nINSTRUMENTATION_STATUS: test=skip\nINSTRUMENTATION_STATUS_CODE: -3\nINSTRUMENTATION_CODE: -1\n",
        );
        assert_eq!(r.tests[0].status, TestStatus::Skipped);
    }

    #[test]
    fn durations_come_from_when_the_status_lines_arrived() {
        let lines: Vec<(f64, String)> = [
            (0.5, "INSTRUMENTATION_STATUS: class=a.B"),
            (0.5, "INSTRUMENTATION_STATUS: test=one"),
            (0.5, "INSTRUMENTATION_STATUS_CODE: 1"),
            (1.75, "INSTRUMENTATION_STATUS: class=a.B"),
            (1.75, "INSTRUMENTATION_STATUS: test=one"),
            (1.75, "INSTRUMENTATION_STATUS_CODE: 0"),
            // No start line: no duration rather than a guess.
            (2.0, "INSTRUMENTATION_STATUS: class=a.B"),
            (2.0, "INSTRUMENTATION_STATUS: test=two"),
            (2.0, "INSTRUMENTATION_STATUS_CODE: -3"),
            (2.0, "INSTRUMENTATION_CODE: -1"),
        ]
        .iter()
        .map(|(t, l)| (*t, format!("{l}\n")))
        .collect();
        let r = parse_timed(&lines);
        assert_eq!(r.tests[0].secs, Some(1.25));
        assert_eq!(r.tests[1].secs, None);
        assert_eq!(r.final_code, Some(-1));
        assert!(parse(PASS).tests.iter().all(|t| t.secs.is_none()));
    }

    #[test]
    fn shards_are_validated_and_passed_to_the_runner() {
        for (count, index) in [(0, 0), (2, 2), (1, 5)] {
            let err = Shard::new(count, index).unwrap_err();
            assert_eq!(errors::code_of(&err), errors::CONFIG, "{count}/{index}");
        }
        let o = Options {
            runner: "com.x.test/androidx.test.runner.AndroidJUnitRunner",
            class: Some("a.B"),
            timeout: 1,
            shard: Some(Shard::new(3, 1).unwrap()),
        };
        assert_eq!(
            instrument_command(&o),
            "am instrument -w -r -e numShards 3 -e shardIndex 1 -e class a.B com.x.test/androidx.test.runner.AndroidJUnitRunner"
        );
        let unsharded = Options { shard: None, ..o };
        assert!(!instrument_command(&unsharded).contains("numShards"));
    }

    #[test]
    fn every_ending_has_its_own_status() {
        let s = |out: &str| Status::of(&parse(out), false, false);
        assert_eq!(s(PASS), Status::Passed);
        // A legitimately empty shard finishes normally with no tests.
        assert_eq!(s("INSTRUMENTATION_CODE: -1\n"), Status::Passed);
        assert_eq!(
            s(
                "INSTRUMENTATION_STATUS: test=x\nINSTRUMENTATION_STATUS_CODE: -2\nINSTRUMENTATION_CODE: -1\n"
            ),
            Status::Failed
        );
        assert_eq!(
            s("INSTRUMENTATION_RESULT: shortMsg=Process crashed.\nINSTRUMENTATION_CODE: 0\n"),
            Status::RunnerError
        );
        assert_eq!(s("INSTRUMENTATION_CODE: 0\n"), Status::RunnerError);
        assert_eq!(
            s("onError: commandError=true message=INSTRUMENTATION_FAILED: com.nope/X\n"),
            Status::NotStarted
        );
        // Missing output is never an empty shard.
        assert_eq!(s(""), Status::Incomplete);
        assert_eq!(s("INSTRUMENTATION_STATUS_CODE: 0\n"), Status::Incomplete);
        assert_eq!(Status::of(&parse(PASS), true, false), Status::TimedOut);
        assert_eq!(Status::of(&parse(PASS), true, true), Status::Cancelled);
    }

    #[test]
    fn argument_validation() {
        assert!(safe(
            "com.x.test/androidx.test.runner.AndroidJUnitRunner",
            ""
        ));
        assert!(!safe("a; rm -rf /", ""));
        assert!(!safe("$(id)", ""));
        assert!(safe("a.B#m,a.C", "#,"));
        assert!(!safe("a.B#m", ""));
    }
}
