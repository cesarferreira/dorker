//! `dorker test`: run an instrumentation runner and report a verdict.

use crate::errors;
use crate::runtime;
use crate::sdk::Sdk;
use crate::store::{State, Store, now_secs};
use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::io::Read;
use std::process::Stdio;
use std::time::{Duration, Instant};

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub passed: u32,
    pub failed: u32,
    pub errors: u32,
    pub ignored: u32,
    pub failures: Vec<Failure>,
    pub final_code: Option<i32>,
    pub crashed: bool,
    pub failed_to_start: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Failure {
    pub class: String,
    pub test: String,
    pub stack: String,
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
pub fn parse(output: &str) -> Report {
    let mut r = Report::default();
    let mut block: HashMap<String, String> = HashMap::new();
    let mut last_key: Option<String> = None;
    for line in output.lines() {
        let line = line.trim_end_matches('\r');
        if let Some(rest) = line.strip_prefix("INSTRUMENTATION_STATUS_CODE:") {
            let code: i32 = rest.trim().parse().unwrap_or(i32::MIN);
            let get = |k: &str| block.get(k).cloned().unwrap_or_default();
            if let Some(err) = block.get("Error") {
                // The activity manager reports "could not start" as a status block.
                r.failed_to_start = Some(err.clone()); // more specific than the onError line
                block.clear();
                last_key = None;
                continue;
            }
            match code {
                0 => r.passed += 1,
                -1 => {
                    r.errors += 1;
                    r.failures.push(Failure {
                        class: get("class"),
                        test: get("test"),
                        stack: get("stack"),
                    });
                }
                -2 => {
                    r.failed += 1;
                    r.failures.push(Failure {
                        class: get("class"),
                        test: get("test"),
                        stack: get("stack"),
                    });
                }
                -3 | -4 => r.ignored += 1,
                _ => {} // 1 = test started
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

pub fn run(key: &str, runner: &str, class: Option<&str>, timeout: u64) -> Result<i32> {
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
    let logdir = store
        .instance_dir(&inst.id)
        .join(format!("test-{}", now_secs()));
    std::fs::create_dir_all(&logdir)?;

    let mut remote = String::from("am instrument -w -r");
    if let Some(c) = class {
        remote.push_str(&format!(" -e class {c}"));
    }
    remote.push(' ');
    remote.push_str(runner);

    let _ = runtime::adb(&sdk, &serial).args(["logcat", "-c"]).output();
    let mut child = runtime::adb(&sdk, &serial)
        .args(["shell", &remote])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("running adb")?;
    let mut out_pipe = child.stdout.take().unwrap();
    let mut err_pipe = child.stderr.take().unwrap();
    let out_t = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = out_pipe.read_to_string(&mut s);
        s
    });
    let err_t = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = err_pipe.read_to_string(&mut s);
        s
    });
    let start = Instant::now();
    let mut timed_out = false;
    loop {
        if child.try_wait()?.is_some() {
            break;
        }
        if start.elapsed() > Duration::from_secs(timeout) || runtime::interrupted() {
            timed_out = true;
            let _ = child.kill();
            let _ = child.wait();
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let stdout = out_t.join().unwrap_or_default();
    let stderr = err_t.join().unwrap_or_default();
    std::fs::write(logdir.join("instrument.txt"), format!("{stdout}{stderr}"))?;
    if let Ok(o) = runtime::adb(&sdk, &serial).args(["logcat", "-d"]).output() {
        let _ = std::fs::write(logdir.join("logcat.txt"), o.stdout);
    }

    // Some failures (e.g. "target has no code") arrive on stderr; parse both.
    let report = parse(&format!("{stdout}{stderr}"));
    println!(
        "Tests: {} passed, {} failed, {} errors, {} ignored",
        report.passed, report.failed, report.errors, report.ignored
    );
    for f in &report.failures {
        println!("\nFAILED {}#{}\n{}", f.class, f.test, f.stack.trim_end());
    }
    if timed_out {
        eprintln!("error: instrumentation did not finish within {timeout}s");
    }
    if let Some(m) = &report.failed_to_start {
        eprintln!("error: {m}");
        // Android's own detail is not always delivered, so show what can be run.
        let listed = runtime::adb(&sdk, &serial)
            .args(["shell", "pm", "list", "instrumentation"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        if listed.is_empty() {
            eprintln!(
                "No instrumentation is installed in this instance. Install a test APK with `dorker install`."
            );
        } else {
            eprintln!("Instrumentation installed in this instance:");
            for l in listed.lines() {
                eprintln!("  {}", l.trim_start_matches("instrumentation:"));
            }
        }
    }
    if report.crashed {
        eprintln!("error: the instrumentation process crashed");
    }
    if report.ok() && !timed_out {
        println!("\nPASSED (logs: {})", logdir.display());
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
