//! `dorker test --image`: run one installed instrumentation suite in fixed shards across a
//! pool of disposable instances started from one image, and report one verdict.
//!
//! Each worker runs exactly one shard: `am instrument -e numShards N -e shardIndex i`, so
//! AndroidJUnitRunner (or a runner with the same contract) decides which tests run where.
//! The shard count never changes after a failure, and nothing is retried.
//!
//! Policy:
//! - Shards run to the end even when other shards have failing tests.
//! - When a worker fails to start, workers still starting are cancelled; shards that are
//!   already running finish, so their results are kept. The run cannot pass.
//! - Ctrl-C / SIGTERM cancels every shard and removes every worker (exit 130).
//! - A shard that timed out, crashed or stopped without the runner's final result is
//!   incomplete; any incomplete shard, a test that ran in two shards (the runner ignored the
//!   shard arguments) or an empty suite fails the run.
//! - Exit codes, first match wins: 130 interrupted; the startup error's code (5, or 8 for
//!   capacity, 3 for a missing dependency); 6 adb failure; 7 tests failed or incomplete;
//!   1 a worker could not be removed; 0 passed.

use crate::errors;
use crate::junit;
use crate::pool::{self, Event, Outcome, Pool, PoolSpec};
use crate::runtime;
use crate::sdk::Sdk;
use crate::store::{Instance, Store};
use crate::testcmd::{self, Execution, Options, Report, Shard, Status, TestStatus};
use anyhow::Result;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

pub const SCHEMA: &str = "dorker.test-run/1";
pub const SHARD_SCHEMA: &str = "dorker.test-shard/1";
const INTERRUPTED: i32 = 130;

pub struct ImageTestArgs {
    pub image: String,
    pub workers: u32,
    pub runner: String,
    pub class: Option<String>,
    /// Seconds each shard's instrumentation may run.
    pub timeout: u64,
    /// Seconds each worker may take to boot.
    pub startup_timeout: u64,
    pub json: bool,
    pub out: Option<PathBuf>,
    /// Start even when the capacity check says the workers do not fit.
    pub force: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShardStatus {
    /// The instrumentation ran; how it ended.
    Ran(Status),
    /// The worker never became ready.
    StartupFailed,
    /// adb could not be run against the worker.
    AdbFailed,
    /// Cancelled before the shard started.
    NotRun,
}

impl ShardStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ShardStatus::Ran(s) => s.as_str(),
            ShardStatus::StartupFailed => "startup_failed",
            ShardStatus::AdbFailed => "adb_failed",
            ShardStatus::NotRun => "not_run",
        }
    }

    /// The runner reached its final result, so the shard's test list is complete.
    pub fn completed(self) -> bool {
        matches!(self, ShardStatus::Ran(Status::Passed | Status::Failed))
    }
}

pub struct ShardResult {
    pub index: u32,
    pub status: ShardStatus,
    pub instance: Option<Instance>,
    pub restored: Option<bool>,
    /// Queue + boot time.
    pub startup_secs: f64,
    /// Boot time alone.
    pub ready_secs: Option<f64>,
    pub test_secs: Option<f64>,
    pub report: Option<Report>,
    pub message: Option<String>,
    /// Exit code of a startup failure.
    pub code: Option<i32>,
    pub dir: PathBuf,
    /// Diagnostics that could not be collected; they never change the verdict.
    pub collection_errors: Vec<String>,
}

/// What a worker's shard produced, collected before the worker is removed.
struct ShardRun {
    exec: Execution,
    message: Option<String>,
    collection_errors: Vec<String>,
}

/// Files a shard may leave in its folder. `failure.png` is taken after a shard did not pass:
/// it shows the screen when the shard ended, not necessarily the failing assertion.
const ARTIFACTS: [&str; 5] = [
    "result.json",
    "instrument.txt",
    "logcat.txt",
    "emulator.log",
    "failure.png",
];

pub struct Aggregate {
    pub verdict: &'static str,
    pub code: i32,
    pub passed: u32,
    pub failed: u32,
    pub errors: u32,
    pub ignored: u32,
    pub started: u32,
    pub completed: u32,
    /// Tests reported by more than one shard: (test id, shard indices).
    pub duplicates: Vec<(String, Vec<u32>)>,
}

/// The run's verdict and exit code from its shards. Pure, so the precedence is unit-tested.
pub fn aggregate(shards: &[ShardResult], cleanup_ok: bool, interrupted: bool) -> Aggregate {
    let mut a = Aggregate {
        verdict: "passed",
        code: 0,
        passed: 0,
        failed: 0,
        errors: 0,
        ignored: 0,
        started: 0,
        completed: 0,
        duplicates: vec![],
    };
    let mut seen: BTreeMap<String, Vec<u32>> = BTreeMap::new();
    for s in shards {
        if s.instance.is_some() && s.status != ShardStatus::StartupFailed {
            a.started += 1;
        }
        if s.status.completed() {
            a.completed += 1;
        }
        if let Some(r) = &s.report {
            a.passed += r.passed;
            a.failed += r.failed;
            a.errors += r.errors;
            a.ignored += r.ignored;
            for t in &r.tests {
                let shards = seen.entry(t.id()).or_default();
                if !shards.contains(&s.index) {
                    shards.push(s.index);
                }
            }
        }
    }
    a.duplicates = seen.into_iter().filter(|(_, v)| v.len() > 1).collect();
    let all_complete = shards.iter().all(|s| s.status.completed());
    let total = a.passed + a.failed + a.errors + a.ignored;
    let (verdict, code) = if interrupted {
        ("cancelled", INTERRUPTED)
    } else if let Some(s) = shards
        .iter()
        .find(|s| s.status == ShardStatus::StartupFailed)
    {
        ("startup_failed", s.code.unwrap_or(errors::STARTUP))
    } else if shards.iter().any(|s| s.status == ShardStatus::AdbFailed) {
        ("adb_failed", errors::ADB)
    } else if !all_complete {
        ("incomplete", errors::TEST)
    } else if !a.duplicates.is_empty() {
        ("duplicate_tests", errors::TEST)
    } else if a.failed + a.errors > 0 {
        ("failed", errors::TEST)
    } else if total == 0 {
        ("empty", errors::TEST)
    } else if !cleanup_ok {
        ("cleanup_failed", 1)
    } else {
        ("passed", 0)
    };
    a.verdict = verdict;
    a.code = code;
    a
}

fn shard_json(s: &ShardResult) -> serde_json::Value {
    let r = s.report.as_ref();
    serde_json::json!({
        "shard": s.index,
        "status": s.status.as_str(),
        "instance": s.instance.as_ref().map(|i| &i.id),
        "name": s.instance.as_ref().map(|i| &i.name),
        "serial": s.instance.as_ref().map(|i| i.serial()),
        "boot": s.restored.map(|r| if r { "snapshot" } else { "cold" }),
        "startup_secs": round(s.startup_secs),
        "ready_secs": s.ready_secs.map(round),
        "test_secs": s.test_secs.map(round),
        "passed": r.map_or(0, |r| r.passed),
        "failed": r.map_or(0, |r| r.failed),
        "errors": r.map_or(0, |r| r.errors),
        "ignored": r.map_or(0, |r| r.ignored),
        "message": s.message,
        "dir": s.dir,
        "artifacts": artifacts_json(&s.dir),
        "collection_errors": s.collection_errors,
    })
}

/// The shard's artifacts that exist, by file name.
fn artifacts_json(dir: &Path) -> serde_json::Value {
    let mut m = serde_json::Map::new();
    for name in ARTIFACTS {
        let p = dir.join(name);
        if p.is_file() || name == "result.json" {
            m.insert(name.into(), serde_json::json!(p));
        }
    }
    serde_json::Value::Object(m)
}

/// `shards/<i>/result.json`: the summary's entry for this shard plus every test it reported.
fn shard_result_json(s: &ShardResult, count: u32, image_id: &str) -> serde_json::Value {
    let mut v = shard_json(s);
    let tests: Vec<_> = s
        .report
        .iter()
        .flat_map(|r| &r.tests)
        .map(|t| {
            serde_json::json!({
                "class": t.class, "test": t.test, "status": t.status.as_str(),
                "secs": t.secs.map(|x| (x * 1000.0).round() / 1000.0),
            })
        })
        .collect();
    let failures: Vec<_> = s
        .report
        .iter()
        .flat_map(|r| &r.failures)
        .map(|f| serde_json::json!({"class": f.class, "test": f.test, "stack": f.stack}))
        .collect();
    let o = v.as_object_mut().unwrap();
    o.insert("schema".into(), SHARD_SCHEMA.into());
    o.insert("shard_count".into(), count.into());
    o.insert("image_id".into(), image_id.into());
    o.insert("tests".into(), tests.into());
    o.insert("failures".into(), failures.into());
    v
}

/// The JUnit report: one suite per shard, plus a `dorker` suite for run-level problems.
fn junit_suites(
    shards: &[ShardResult],
    agg: &Aggregate,
    count: u32,
    image_id: &str,
    survivors: &[(String, String)],
) -> Vec<junit::Suite> {
    use junit::{Case, Outcome, Suite};
    let mut suites = Vec::new();
    for s in shards {
        let mut cases = Vec::new();
        if let Some(r) = &s.report {
            for t in &r.tests {
                let stack = || {
                    r.failures
                        .iter()
                        .find(|f| f.class == t.class && f.test == t.test)
                        .map_or(String::new(), |f| f.stack.clone())
                };
                let outcome = match t.status {
                    TestStatus::Passed => Outcome::Passed,
                    TestStatus::Skipped => Outcome::Skipped,
                    TestStatus::Failed => {
                        let detail = stack();
                        Outcome::Failure {
                            message: junit::headline(&detail),
                            detail,
                        }
                    }
                    TestStatus::Error => {
                        let detail = stack();
                        Outcome::Error {
                            message: junit::headline(&detail),
                            detail,
                        }
                    }
                };
                cases.push(Case {
                    class: t.class.clone(),
                    name: t.test.clone(),
                    secs: t.secs,
                    outcome,
                });
            }
        }
        if !s.status.completed() {
            let detail = s.message.clone().unwrap_or_default();
            cases.push(Case {
                class: "dorker".into(),
                name: format!("shard {} {}", s.index, s.status.as_str()),
                secs: None,
                outcome: Outcome::Error {
                    message: junit::headline(&detail),
                    detail,
                },
            });
        }
        let mut properties = vec![
            ("dorker.shard".to_string(), s.index.to_string()),
            ("dorker.shard_count".to_string(), count.to_string()),
            ("dorker.status".to_string(), s.status.as_str().to_string()),
            ("dorker.image".to_string(), image_id.to_string()),
        ];
        if let Some(i) = &s.instance {
            properties.push(("dorker.instance".to_string(), i.id.clone()));
        }
        suites.push(Suite {
            name: format!("shard-{}", s.index),
            hostname: s.instance.as_ref().map(|i| i.serial()),
            secs: s.test_secs,
            properties,
            cases,
        });
    }
    let mut run = Vec::new();
    for (t, on) in &agg.duplicates {
        run.push(Case {
            class: "dorker".into(),
            name: format!("duplicate {t}"),
            secs: None,
            outcome: Outcome::Error {
                message: format!("{t} ran in shards {on:?}"),
                detail: "the runner does not seem to honour numShards/shardIndex".into(),
            },
        });
    }
    for (id, e) in survivors {
        run.push(Case {
            class: "dorker".into(),
            name: format!("cleanup {id}"),
            secs: None,
            outcome: Outcome::Error {
                message: format!("could not remove worker instance {id}"),
                detail: e.clone(),
            },
        });
    }
    if !run.is_empty() {
        suites.push(Suite {
            name: "dorker".into(),
            hostname: None,
            secs: None,
            properties: vec![],
            cases: run,
        });
    }
    suites
}

fn write_junit(path: &Path, run_id: &str, suites: &[junit::Suite], secs: f64) -> Result<()> {
    let tmp = path.with_extension("xml.tmp");
    std::fs::write(
        &tmp,
        junit::render(&format!("dorker {run_id}"), suites, secs),
    )?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Keep what a worker leaves behind before it is removed: its emulator log, and a screenshot
/// when the shard did not pass. Each step is bounded; failures are recorded, never fatal.
fn collect(
    sdk: &Sdk,
    serial: &str,
    instance_dir: &Path,
    dir: &Path,
    status: Status,
) -> Vec<String> {
    let mut errors = Vec::new();
    if let Err(e) = std::fs::copy(instance_dir.join("emulator.log"), dir.join("emulator.log")) {
        errors.push(format!("emulator.log: {e}"));
    }
    if matches!(status, Status::Passed | Status::Cancelled) {
        return errors;
    }
    let limit = testcmd::collect_timeout();
    match testcmd::output_within(
        runtime::adb(sdk, serial).args(["exec-out", "screencap", "-p"]),
        limit,
    ) {
        Some(o) if o.stdout.starts_with(b"\x89PNG\r\n\x1a\n") => {
            if let Err(e) = std::fs::write(dir.join("failure.png"), &o.stdout) {
                errors.push(format!("failure.png: {e}"));
            }
        }
        Some(_) => errors.push("failure.png: screencap did not return a PNG".into()),
        None => errors.push(format!(
            "failure.png: screencap did not finish within {}s",
            limit.as_secs()
        )),
    }
    errors
}

fn round(s: f64) -> f64 {
    (s * 100.0).round() / 100.0
}

#[allow(clippy::too_many_arguments)]
fn summary_json(
    a: &ImageTestArgs,
    pool: &Pool,
    shards: &[ShardResult],
    agg: &Aggregate,
    cleanup: serde_json::Value,
    dir: &Path,
    elapsed: f64,
    junit: junit::Counts,
) -> serde_json::Value {
    let failures: Vec<_> = shards
        .iter()
        .filter_map(|s| s.report.as_ref().map(|r| (s.index, r)))
        .flat_map(|(i, r)| {
            r.failures.iter().map(move |f| {
                serde_json::json!({"shard": i, "class": f.class, "test": f.test, "stack": f.stack})
            })
        })
        .collect();
    serde_json::json!({
        "schema": SCHEMA,
        "ok": agg.code == 0,
        "verdict": agg.verdict,
        "exit_code": agg.code,
        "run_id": pool.run_id,
        "image": {"ref": a.image, "id": pool.image.id},
        "runner": a.runner,
        "class": a.class,
        "shards": {"requested": pool.workers, "started": agg.started, "completed": agg.completed},
        "passed": agg.passed,
        "failed": agg.failed,
        "errors": agg.errors,
        "ignored": agg.ignored,
        "failures": failures,
        "duplicates": agg.duplicates.iter().map(|(t, s)| serde_json::json!({"test": t, "shards": s})).collect::<Vec<_>>(),
        "workers": shards.iter().map(shard_json).collect::<Vec<_>>(),
        "cleanup": cleanup,
        "junit": junit,
        "results_dir": dir,
        "elapsed_secs": round(elapsed),
    })
}

fn write_json(path: &Path, v: &serde_json::Value) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(v)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// A new, empty folder for this run's results: `<out or <store>/results>/<run-id>`.
fn allocate_dir(store: &Store, out: Option<&Path>, run_id: &str) -> Result<PathBuf> {
    let base = out.map_or_else(|| store.root().join("results"), Path::to_path_buf);
    std::fs::create_dir_all(&base)
        .map_err(|e| anyhow::anyhow!("creating {}: {e}", base.display()))?;
    let dir = base.join(run_id);
    std::fs::create_dir(&dir).map_err(|e| {
        errors::with(errors::CONFLICT)(anyhow::anyhow!(
            "refusing to reuse results folder {}: {e}",
            dir.display()
        ))
    })?;
    Ok(dir)
}

fn check_runner(runner: &str, workers: u32) -> Result<()> {
    let (package, class) = runner.split_once('/').unwrap_or((runner, ""));
    if package.starts_with("androidx.test.orchestrator") || class.contains("Orchestrator") {
        return Err(errors::with(errors::CONFIG)(anyhow::anyhow!(
            "{runner} is Android Test Orchestrator, which --image mode does not support; give the test APK's own runner (e.g. <test package>/androidx.test.runner.AndroidJUnitRunner)"
        )));
    }
    if workers > 1 && class != "androidx.test.runner.AndroidJUnitRunner" {
        eprintln!(
            "note: {runner} must split the suite with AndroidJUnitRunner's numShards/shardIndex arguments (subclasses of AndroidJUnitRunner do); a test reported by two shards fails the run"
        );
    }
    Ok(())
}

pub fn run(a: ImageTestArgs) -> Result<i32> {
    testcmd::validate(&a.runner, a.class.as_deref())?;
    check_runner(&a.runner, a.workers)?;
    let t0 = Instant::now();
    let store = Store::open()?;
    let sdk = Sdk::locate().map_err(errors::with(errors::DEPENDENCY))?;
    let (mut pool, notes) = Pool::prepare(
        store,
        sdk,
        PoolSpec {
            image: a.image.clone(),
            workers: a.workers,
            startup_timeout: a.startup_timeout,
            pull: false,
            reserve_mb: pool::host_reserve_mb(),
            force: a.force,
        },
        pool::probe,
    )?;
    for n in &notes {
        eprintln!("warning: {n}");
    }
    let dir = match allocate_dir(pool.store(), a.out.as_deref(), &pool.run_id) {
        Ok(d) => d,
        Err(e) => {
            pool.cleanup();
            return Err(e);
        }
    };
    let shard_dir = |i: u32| dir.join("shards").join(i.to_string());
    let summary_path = dir.join("summary.json");
    let junit_path = dir.join("junit.xml");
    // Written before anything starts, so even a killed run leaves its identity behind.
    let mut running = summary_json(
        &a,
        &pool,
        &[],
        &aggregate(&[], true, false),
        serde_json::json!({"status": "pending"}),
        &dir,
        0.0,
        junit::Counts::default(),
    );
    running["verdict"] = "running".into();
    running["exit_code"] = serde_json::Value::Null;
    running["ok"] = false.into();
    if let Err(e) = write_json(&summary_path, &running) {
        pool.cleanup();
        return Err(e.context(format!("writing {}", summary_path.display())));
    }
    eprintln!(
        "Running {} on {} worker(s) from image {} ({}); results in {}",
        a.runner,
        a.workers,
        a.image,
        pool.image.id,
        dir.display()
    );

    let events = |e: &Event| match e {
        Event::Starting { worker } => eprintln!("[shard {worker}] starting"),
        Event::Ready {
            worker,
            inst,
            ready_secs,
            restored,
        } => eprintln!(
            "[shard {worker}] ready on {} in {ready_secs:.1}s ({})",
            inst.serial(),
            if *restored {
                "snapshot restore"
            } else {
                "cold boot"
            }
        ),
        Event::StartFailed { worker, error } => eprintln!(
            "[shard {worker}] failed to start: {}",
            format!("{error:#}").lines().next().unwrap_or("")
        ),
        Event::Cancelled { worker } => eprintln!("[shard {worker}] cancelled"),
        Event::Removed { worker, instance } => {
            crate::log::vlog!("[shard {worker}] removed instance {instance}")
        }
    };
    let workers = pool.workers;
    let store = pool.store();
    let results = pool.run(&events, |w, cancelled| -> Result<ShardRun> {
        let shard = Shard::new(workers, w.index)?;
        let opts = Options {
            runner: &a.runner,
            class: a.class.as_deref(),
            timeout: a.timeout,
            shard: Some(shard),
        };
        let serial = w.started.inst.serial();
        let sdir = shard_dir(w.index);
        let exec = testcmd::execute(&w.started.sdk, &serial, &opts, &sdir, cancelled)?;
        let message = match exec.status {
            Status::NotStarted => Some(format!(
                "{}\n{}",
                exec.report.failed_to_start.clone().unwrap_or_default(),
                testcmd::explain_not_started(&w.started.sdk, &serial)
            )),
            Status::RunnerError => {
                Some("the instrumentation process crashed or ended abnormally".into())
            }
            Status::Incomplete => {
                Some("the output stopped before the runner's final result".into())
            }
            Status::TimedOut => Some(format!("did not finish within {}s", a.timeout)),
            _ => None,
        };
        eprintln!(
            "[shard {}] {}: {} passed, {} failed, {} errors, {} ignored in {:.1}s",
            w.index,
            exec.status.as_str(),
            exec.report.passed,
            exec.report.failed,
            exec.report.errors,
            exec.report.ignored,
            exec.secs
        );
        let mut collection_errors = exec.collection_errors.clone();
        collection_errors.extend(collect(
            &w.started.sdk,
            &serial,
            &store.instance_dir(&w.started.inst.id),
            &sdir,
            exec.status,
        ));
        Ok(ShardRun {
            exec,
            message,
            collection_errors,
        })
    });

    let mut shards = Vec::new();
    for r in results {
        let d = shard_dir(r.index);
        let _ = std::fs::create_dir_all(&d);
        let mut s = ShardResult {
            index: r.index,
            status: ShardStatus::NotRun,
            instance: None,
            restored: None,
            startup_secs: r.startup_secs,
            ready_secs: None,
            test_secs: None,
            report: None,
            message: None,
            code: None,
            dir: d.clone(),
            collection_errors: vec![],
        };
        match r.outcome {
            Outcome::Completed {
                inst,
                ready_secs,
                restored,
                value,
            } => {
                s.instance = Some(inst);
                s.restored = Some(restored);
                s.ready_secs = Some(ready_secs);
                match value {
                    Ok(run) => {
                        s.status = ShardStatus::Ran(run.exec.status);
                        s.test_secs = Some(run.exec.secs);
                        s.report = Some(run.exec.report);
                        s.message = run.message;
                        s.collection_errors = run.collection_errors;
                    }
                    Err(e) => {
                        s.status = ShardStatus::AdbFailed;
                        s.message = Some(format!("{e:#}"));
                    }
                }
            }
            Outcome::StartFailed { error, inst } => {
                s.status = ShardStatus::StartupFailed;
                s.code = Some(errors::code_of(&error));
                s.message = Some(format!("{error:#}"));
                s.instance = inst;
            }
            Outcome::Cancelled { inst } => s.instance = inst,
        }
        // A worker that never ran a shard is still around until cleanup: keep its log.
        if let (false, Some(i)) = (d.join("emulator.log").is_file(), &s.instance) {
            let log = pool.store().instance_dir(&i.id).join("emulator.log");
            if log.is_file() {
                if let Err(e) = std::fs::copy(log, d.join("emulator.log")) {
                    s.collection_errors.push(format!("emulator.log: {e}"));
                }
            }
        }
        // On slow hosts the emulator logs a completed restore only after the device is
        // ready, so the full log copied afterwards is the better witness.
        if s.restored == Some(false)
            && std::fs::read_to_string(d.join("emulator.log"))
                .is_ok_and(|t| t.contains("Successfully loaded snapshot"))
        {
            s.restored = Some(true);
        }
        if let Err(e) = write_json(
            &d.join("result.json"),
            &shard_result_json(&s, workers, &pool.image.id),
        ) {
            s.collection_errors.push(format!("result.json: {e:#}"));
        }
        shards.push(s);
    }

    let interrupted = runtime::interrupted();
    let pending = aggregate(&shards, true, interrupted);
    let suites = junit_suites(&shards, &pending, workers, &pool.image.id, &[]);
    let _ = write_junit(
        &junit_path,
        &pool.run_id,
        &suites,
        t0.elapsed().as_secs_f64(),
    );
    let _ = write_json(
        &summary_path,
        &summary_json(
            &a,
            &pool,
            &shards,
            &pending,
            serde_json::json!({"status": "pending"}),
            &dir,
            t0.elapsed().as_secs_f64(),
            junit::counts(&suites),
        ),
    );
    let cleanup = pool.cleanup();
    let agg = aggregate(&shards, cleanup.ok(), interrupted);
    let cleanup_json = serde_json::json!({
        "status": if cleanup.ok() { "ok" } else { "failed" },
        "removed": cleanup.removed,
        "survivors": cleanup.survivors.iter().map(|(i, e)| serde_json::json!({"instance": i, "error": e})).collect::<Vec<_>>(),
    });
    let elapsed = t0.elapsed().as_secs_f64();
    let suites = junit_suites(&shards, &agg, workers, &pool.image.id, &cleanup.survivors);
    write_junit(&junit_path, &pool.run_id, &suites, elapsed)?;
    let summary = summary_json(
        &a,
        &pool,
        &shards,
        &agg,
        cleanup_json,
        &dir,
        elapsed,
        junit::counts(&suites),
    );
    write_json(&summary_path, &summary)?;

    if a.json {
        println!("{}", serde_json::to_string_pretty(&summary)?);
    } else {
        print_table(&shards, &agg, pool.workers, elapsed, &dir);
    }
    for (id, e) in &cleanup.survivors {
        eprintln!("error: could not remove worker instance {id}: {e}");
    }
    for s in &shards {
        if let (
            ShardStatus::StartupFailed | ShardStatus::AdbFailed | ShardStatus::Ran(_),
            Some(m),
        ) = (s.status, &s.message)
        {
            eprintln!("error: shard {}: {m}", s.index);
        }
        for e in &s.collection_errors {
            eprintln!("warning: shard {}: could not collect {e}", s.index);
        }
    }
    for (t, s) in &agg.duplicates {
        eprintln!(
            "error: {t} ran in shards {s:?}: the runner does not seem to honour numShards/shardIndex"
        );
    }
    match agg.verdict {
        "passed" => {}
        "empty" => eprintln!(
            "error: no tests ran in any shard (does --class match anything, and is the test APK installed in the image?)"
        ),
        v => eprintln!("\nFAILED ({v}). Results in {}", dir.display()),
    }
    Ok(agg.code)
}

fn print_table(shards: &[ShardResult], agg: &Aggregate, requested: u32, secs: f64, dir: &Path) {
    let st = crate::style::Style::stdout();
    println!(
        "{}",
        st.dim(&format!(
            "{:<7}{:<16}{:<16}{:<15}{:>7}{:>7}{:>7}{:>8}{:>9}",
            "SHARD", "SERIAL", "BOOT", "STATUS", "PASSED", "FAILED", "ERRORS", "IGNORED", "TIME"
        ))
    );
    for s in shards {
        let r = s.report.as_ref();
        let boot = match s.restored {
            Some(true) => format!("snapshot {:.1}s", s.startup_secs),
            Some(false) => format!("cold {:.1}s", s.startup_secs),
            None => "-".into(),
        };
        let status = format!("{:<15}", s.status.as_str());
        println!(
            "{:<7}{:<16}{:<16}{}{:>7}{:>7}{:>7}{:>8}{:>9}",
            s.index,
            s.instance.as_ref().map_or("-".into(), |i| i.serial()),
            boot,
            if s.status == ShardStatus::Ran(Status::Passed) {
                st.green(&status)
            } else {
                st.red_bold(&status)
            },
            r.map_or(0, |r| r.passed),
            r.map_or(0, |r| r.failed),
            r.map_or(0, |r| r.errors),
            r.map_or(0, |r| r.ignored),
            s.test_secs.map_or("-".into(), |t| format!("{t:.1}s")),
        );
    }
    println!(
        "\nTests: {} passed, {} failed, {} errors, {} ignored ({}/{requested} shards completed, {secs:.1}s)",
        agg.passed, agg.failed, agg.errors, agg.ignored, agg.completed
    );
    for s in shards {
        for f in s.report.iter().flat_map(|r| &r.failures) {
            println!(
                "\nFAILED {}#{} (shard {})\n{}",
                f.class,
                f.test,
                s.index,
                f.stack.trim_end()
            );
        }
    }
    if agg.code == 0 {
        println!("\nPASSED (results: {})", dir.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testcmd::{TestCase, TestStatus};

    fn report(tests: &[(&str, TestStatus)]) -> Report {
        let mut r = Report {
            final_code: Some(-1),
            ..Default::default()
        };
        for (name, status) in tests {
            match status {
                TestStatus::Passed => r.passed += 1,
                TestStatus::Failed => r.failed += 1,
                TestStatus::Error => r.errors += 1,
                TestStatus::Skipped => r.ignored += 1,
            }
            r.tests.push(TestCase {
                class: "a.B".into(),
                test: name.to_string(),
                status: *status,
                secs: None,
            });
        }
        r
    }

    fn shard(index: u32, status: ShardStatus, r: Option<Report>) -> ShardResult {
        ShardResult {
            index,
            status,
            instance: None,
            restored: None,
            startup_secs: 0.0,
            ready_secs: None,
            test_secs: None,
            report: r,
            message: None,
            code: None,
            dir: PathBuf::new(),
            collection_errors: vec![],
        }
    }

    fn passed(index: u32, tests: &[&str]) -> ShardResult {
        let t: Vec<_> = tests.iter().map(|t| (*t, TestStatus::Passed)).collect();
        shard(index, ShardStatus::Ran(Status::Passed), Some(report(&t)))
    }

    #[test]
    fn all_shards_passing_and_an_empty_shard_pass() {
        let a = aggregate(
            &[
                passed(0, &["one", "two"]),
                passed(1, &[]),
                passed(2, &["three"]),
            ],
            true,
            false,
        );
        assert_eq!((a.verdict, a.code), ("passed", 0));
        assert_eq!((a.passed, a.completed), (3, 3));
    }

    #[test]
    fn a_failure_in_one_shard_fails_the_run_with_the_test_code() {
        let failing = shard(
            1,
            ShardStatus::Ran(Status::Failed),
            Some(report(&[("boom", TestStatus::Failed)])),
        );
        let a = aggregate(&[passed(0, &["one"]), failing], true, false);
        assert_eq!((a.verdict, a.code), ("failed", 7));
        assert_eq!((a.passed, a.failed), (1, 1));
    }

    #[test]
    fn an_incomplete_shard_never_passes_even_when_its_tests_did() {
        for status in [
            Status::TimedOut,
            Status::Incomplete,
            Status::RunnerError,
            Status::NotStarted,
            Status::Cancelled,
        ] {
            let partial = shard(
                1,
                ShardStatus::Ran(status),
                Some(report(&[("half", TestStatus::Passed)])),
            );
            let a = aggregate(&[passed(0, &["one"]), partial], true, false);
            assert_eq!((a.verdict, a.code), ("incomplete", 7), "{status:?}");
            assert_eq!(a.completed, 1);
        }
        let a = aggregate(
            &[passed(0, &["one"]), shard(1, ShardStatus::NotRun, None)],
            true,
            false,
        );
        assert_eq!(a.code, 7);
    }

    #[test]
    fn an_entirely_empty_suite_fails() {
        let a = aggregate(&[passed(0, &[]), passed(1, &[])], true, false);
        assert_eq!((a.verdict, a.code), ("empty", 7));
    }

    #[test]
    fn a_test_in_two_shards_means_the_runner_ignored_sharding() {
        let a = aggregate(
            &[passed(0, &["one", "two"]), passed(1, &["one", "two"])],
            true,
            false,
        );
        assert_eq!((a.verdict, a.code), ("duplicate_tests", 7));
        assert_eq!(a.duplicates.len(), 2);
        assert_eq!(a.duplicates[0], ("a.B#one".to_string(), vec![0, 1]));
    }

    #[test]
    fn failures_have_a_fixed_precedence() {
        let mut startup = shard(2, ShardStatus::StartupFailed, None);
        startup.code = Some(errors::CONFLICT);
        let adb = shard(1, ShardStatus::AdbFailed, None);
        let failing = shard(
            0,
            ShardStatus::Ran(Status::Failed),
            Some(report(&[("boom", TestStatus::Failed)])),
        );
        let all = [failing, adb, startup];
        assert_eq!(aggregate(&all, false, true).code, 130, "interrupted first");
        let a = aggregate(&all, false, false);
        assert_eq!(
            (a.verdict, a.code),
            ("startup_failed", 8),
            "the startup error's own code"
        );
        assert_eq!(aggregate(&all[..2], false, false).code, 6);
        assert_eq!(aggregate(&all[..1], false, false).code, 7);
        let a = aggregate(&[passed(0, &["one"])], false, false);
        assert_eq!(
            (a.verdict, a.code),
            ("cleanup_failed", 1),
            "never hidden by a pass"
        );
    }

    #[test]
    fn orchestrator_is_refused_and_custom_runners_are_allowed() {
        assert!(check_runner("androidx.test.orchestrator/.AndroidTestOrchestrator", 2).is_err());
        assert!(check_runner("com.x.test/androidx.test.runner.AndroidJUnitRunner", 4).is_ok());
        assert!(check_runner("com.x.test/com.x.HiltRunner", 4).is_ok());
    }

    #[test]
    fn a_results_folder_is_never_reused() {
        let d = crate::testutil::Dir::new();
        let store = Store::at(d.path().join("home")).unwrap();
        let out = d.path().join("out");
        let dir = allocate_dir(&store, Some(&out), "abc").unwrap();
        assert_eq!(dir, out.join("abc"));
        let err = allocate_dir(&store, Some(&out), "abc").unwrap_err();
        assert_eq!(errors::code_of(&err), errors::CONFLICT);
        assert_eq!(
            allocate_dir(&store, None, "def").unwrap(),
            d.path().join("home/results/def")
        );
    }
}
