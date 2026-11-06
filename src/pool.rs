//! Worker pools: a bounded group of disposable instances started from one image, owned by
//! one run, and removed again after work, failure or cancellation. Internal for now; the
//! parallel `dorker test --image` mode is its user.
//!
//! Ownership. A run gets a unique id and writes `runs/<run-id>/manifest.json` (owner pid,
//! image id, worker count, per-worker resource estimate) before any instance exists. Each
//! instance it creates carries `owner = {run_id, worker}` in its `instance.json`, saved
//! before the emulator is spawned, so even a partially started instance can be found.
//! Cleanup only ever touches instances whose owner is this run, under their operation lock,
//! after re-reading them and checking the process identity. A run killed with SIGKILL
//! leaves its manifest behind; the next pool sees that the owner pid is gone and removes
//! that run's instances (and nothing else).
//!
//! Worker states:
//!
//! ```text
//! queued -> admitted -> starting -> ready -> running -> finished -> removed
//!              \            \                    \
//!               +-> start failed / cancelled ----+-> (kept until `cleanup`) -> removed
//! ```
//!
//! Admission. Before starting, the pool estimates what the whole group needs (host memory,
//! disk for the clones, console/ADB port pairs, vCPUs) and compares it with what the host has
//! now, minus what other live pools have claimed but not yet used. It rechecks before each
//! worker starts. The memory figure is an estimate (an emulator uses 1.5-1.8x its guest RAM
//! on the host, see docs/benchmarks.md), not a guarantee against running out of memory.
//! Already running instances are not subtracted again: the host's available memory and free
//! disk already reflect them.

// No command uses the pool yet.
#![allow(dead_code)]

use crate::commands::{self, OP_TIMEOUT, RunArgs, Started};
use crate::errors;
use crate::images::{self, ImageMeta};
use crate::runtime;
use crate::sdk::Sdk;
use crate::store::{Instance, RunOwner, State, Store, new_id, now_secs};
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const MIB: u64 = 1024 * 1024;

/// Host memory kept free for everything that is not an emulator, unless
/// `DORKER_HOST_RESERVE_MB` says otherwise.
pub const DEFAULT_HOST_RESERVE_MB: u64 = 2048;

/// Room a worker's userdata may grow into on top of its clone.
const USERDATA_GROWTH_BYTES: u64 = 256 * MIB;

pub fn host_reserve_mb() -> u64 {
    std::env::var("DORKER_HOST_RESERVE_MB")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(DEFAULT_HOST_RESERVE_MB)
}

/// Estimated host memory of one emulator with `guest_mb` of guest RAM. Measured footprints
/// are 1.5-1.8x the guest RAM (2 GB guest: 3.0-3.7 GB on Linux, 2.2-3.4 GB on macOS).
pub fn estimate_host_mb(guest_mb: u64) -> u64 {
    guest_mb * 3 / 2 + 512
}

// ---------- admission (pure) ----------

/// What the host has right now. Injectable, so admission is unit-tested without a host.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostSnapshot {
    /// `None` when the platform does not report it; admission then warns instead of guessing.
    pub available_memory_mb: Option<u64>,
    pub free_disk_bytes: Option<u64>,
    pub free_port_pairs: usize,
    pub host_cpus: u32,
    /// vCPUs of instances that already have (or are about to have) an emulator process.
    pub running_cpus: u32,
}

/// Capacity claimed but not yet visible in a `HostSnapshot`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Claim {
    pub workers: u32,
    pub memory_mb: u64,
    pub disk_bytes: u64,
    pub ports: usize,
    pub cpus: u32,
}

impl std::ops::Add for Claim {
    type Output = Claim;
    fn add(self, o: Claim) -> Claim {
        Claim {
            workers: self.workers + o.workers,
            memory_mb: self.memory_mb + o.memory_mb,
            disk_bytes: self.disk_bytes + o.disk_bytes,
            ports: self.ports + o.ports,
            cpus: self.cpus + o.cpus,
        }
    }
}

/// Per-worker needs of one pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerCost {
    pub guest_memory_mb: u64,
    pub host_memory_mb: u64,
    pub cpus: u32,
    pub disk_bytes: u64,
}

impl WorkerCost {
    pub fn for_image(store: &Store, meta: &ImageMeta) -> Self {
        // APFS clones are nearly free; elsewhere an instance may be a full copy of the image.
        let clone = if cfg!(target_os = "macos") {
            0
        } else {
            images::dir_usage(&images::image_dir(store, &meta.id)).1
        };
        WorkerCost {
            guest_memory_mb: meta.memory_mb,
            host_memory_mb: estimate_host_mb(meta.memory_mb),
            cpus: meta.cpus,
            disk_bytes: clone + USERDATA_GROWTH_BYTES,
        }
    }

    /// The claim of `n` workers that have not been created yet.
    pub fn fresh(&self, n: u32) -> Claim {
        Claim {
            workers: n,
            memory_mb: self.host_memory_mb * n as u64,
            disk_bytes: self.disk_bytes * n as u64,
            ports: n as usize,
            cpus: self.cpus * n,
        }
    }
}

/// How a pool's workers claim capacity, given the instances that exist. A worker with no
/// instance yet claims everything; one that is created or booting has its port, clone and
/// process already (visible on the host) but not its memory; a READY or finished worker
/// claims nothing more.
pub fn claim_of(m: &Manifest, instances: &[Instance]) -> Claim {
    let mut c = Claim::default();
    for w in 0..m.workers {
        if m.released.contains(&w) {
            continue;
        }
        let inst = instances.iter().find(|i| {
            i.owner
                .as_ref()
                .is_some_and(|o| o.run_id == m.run_id && o.worker == w)
        });
        match inst {
            None => c = c + m.cost.fresh(1),
            Some(i) if matches!(i.state, State::Created) || i.state.expects_process() => {
                if i.state != State::Ready {
                    c.workers += 1;
                    c.memory_mb += m.cost.host_memory_mb;
                }
            }
            Some(_) => {}
        }
    }
    c
}

/// Decide whether `ours` fits next to `others`. `Ok` carries warnings (unknown metrics, CPU
/// oversubscription); `Err` is an actionable capacity message.
pub fn admit(
    host: &HostSnapshot,
    ours: Claim,
    others: Claim,
    cost: &WorkerCost,
    reserve_mb: u64,
    min_free_bytes: u64,
) -> std::result::Result<Vec<String>, String> {
    let mut warnings = Vec::new();
    let mut problems = Vec::new();
    let mut fits = ours.workers;
    let total = ours + others;

    match host.available_memory_mb {
        Some(avail) => {
            let usable = avail.saturating_sub(reserve_mb + others.memory_mb);
            if usable < ours.memory_mb {
                let n = (usable / cost.host_memory_mb.max(1)) as u32;
                fits = fits.min(n);
                problems.push(format!(
                    "memory: {} worker(s) need about {} MB ({} MB each for {} MB of guest RAM), but {avail} MB is available, {reserve_mb} MB is kept for the host{}",
                    ours.workers,
                    ours.memory_mb,
                    cost.host_memory_mb,
                    cost.guest_memory_mb,
                    if others.memory_mb > 0 {
                        format!(" and {} MB is claimed by other runs", others.memory_mb)
                    } else {
                        String::new()
                    }
                ));
            }
        }
        None => warnings.push(format!(
            "could not read the host's available memory; admitting {} worker(s) on the estimate of about {} MB each",
            ours.workers, cost.host_memory_mb
        )),
    }

    match host.free_disk_bytes {
        Some(free) => {
            let usable = free.saturating_sub(min_free_bytes + others.disk_bytes);
            if usable < ours.disk_bytes {
                let n = (usable / cost.disk_bytes.max(1)) as u32;
                fits = fits.min(n);
                problems.push(format!(
                    "disk: {} worker(s) need about {} MB, but {} MB is free and {} MB must stay free{}",
                    ours.workers,
                    ours.disk_bytes / MIB,
                    free / MIB,
                    min_free_bytes / MIB,
                    if others.disk_bytes > 0 {
                        format!(" and {} MB is claimed by other runs", others.disk_bytes / MIB)
                    } else {
                        String::new()
                    }
                ));
            }
        }
        None => warnings.push(
            "could not read the free disk space; not checking it for the workers".to_string(),
        ),
    }

    let ports = host.free_port_pairs.saturating_sub(others.ports);
    if ports < ours.ports {
        fits = fits.min(ports as u32);
        problems.push(format!(
            "ports: {} worker(s) need {} emulator port pairs, but only {} are free ({}..={})",
            ours.workers,
            ours.ports,
            ports,
            runtime::FIRST_PORT,
            runtime::LAST_PORT
        ));
    }

    if host.host_cpus > 0 && host.running_cpus + total.cpus > host.host_cpus {
        warnings.push(format!(
            "{} vCPUs on {} host cores: emulators will compete for CPU and boot slower",
            host.running_cpus + total.cpus,
            host.host_cpus
        ));
    }

    if problems.is_empty() {
        return Ok(warnings);
    }
    let hint = if fits > 0 && fits < ours.workers {
        format!("at most {fits} worker(s) fit now; use --workers {fits}, ")
    } else {
        String::new()
    };
    Err(format!(
        "not enough capacity for {} worker(s):\n  {}\n\nThis is an estimate, not a measurement. {hint}stop other instances (`dorker ps`), or lower the reserve with DORKER_HOST_RESERVE_MB.",
        ours.workers,
        problems.join("\n  ")
    ))
}

// ---------- host probes ----------

/// `MemAvailable` from `/proc/meminfo`, in MB.
pub fn parse_meminfo(text: &str) -> Option<u64> {
    text.lines()
        .find_map(|l| l.strip_prefix("MemAvailable:"))
        .and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok())
        .map(|kb| kb / 1024)
}

/// Free + inactive + speculative + purgeable pages from macOS `vm_stat`, in MB.
pub fn parse_vm_stat(text: &str) -> Option<u64> {
    let page: u64 = text
        .lines()
        .next()?
        .split("page size of ")
        .nth(1)?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    let pages = |label: &str| -> Option<u64> {
        text.lines()
            .find_map(|l| l.strip_prefix(label))
            .and_then(|v| v.trim().trim_end_matches('.').parse().ok())
    };
    let free = pages("Pages free:")?;
    let rest: u64 = ["Pages inactive:", "Pages speculative:", "Pages purgeable:"]
        .iter()
        .filter_map(|l| pages(l))
        .sum();
    Some((free + rest) * page / MIB)
}

pub fn available_memory_mb() -> Option<u64> {
    if let Ok(t) = std::fs::read_to_string("/proc/meminfo") {
        return parse_meminfo(&t);
    }
    let out = std::process::Command::new("vm_stat").output().ok()?;
    out.status
        .success()
        .then(|| parse_vm_stat(&String::from_utf8_lossy(&out.stdout)))
        .flatten()
}

pub fn probe(store: &Store) -> HostSnapshot {
    let running_cpus = store
        .load_all()
        .unwrap_or_default()
        .iter()
        .filter(|i| i.state.expects_process())
        .map(|i| i.cpus)
        .sum();
    HostSnapshot {
        available_memory_mb: available_memory_mb(),
        free_disk_bytes: runtime::free_space_bytes(&store.root()),
        free_port_pairs: runtime::free_port_pairs(store),
        host_cpus: std::thread::available_parallelism().map_or(1, |n| n.get() as u32),
        running_cpus,
    }
}

// ---------- manifests ----------

/// A run's ownership record and resource claim, in `runs/<run-id>/manifest.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub run_id: String,
    pub owner_pid: u32,
    pub created_at: u64,
    pub image_id: String,
    pub workers: u32,
    pub cost: WorkerCost,
    /// Workers that no longer claim capacity (removed, or never going to start).
    #[serde(default)]
    pub released: Vec<u32>,
}

fn manifest_path(store: &Store, run_id: &str) -> PathBuf {
    store.runs_dir().join(run_id).join("manifest.json")
}

fn save_manifest(store: &Store, m: &Manifest) -> Result<()> {
    let path = manifest_path(store, &m.run_id);
    std::fs::create_dir_all(path.parent().unwrap())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(m)?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

pub fn load_manifests(store: &Store) -> Vec<Manifest> {
    let Ok(entries) = std::fs::read_dir(store.runs_dir()) else {
        return vec![];
    };
    let mut out = Vec::new();
    for e in entries.flatten() {
        let path = e.path().join("manifest.json");
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        match serde_json::from_slice::<Manifest>(&bytes) {
            Ok(m) => out.push(m),
            Err(err) => eprintln!("warning: skipping unreadable {}: {err}", path.display()),
        }
    }
    out
}

fn remove_run_dir(store: &Store, run_id: &str) {
    let dir = store.runs_dir().join(run_id);
    if dir.parent() == Some(store.runs_dir().as_path()) && !run_id.is_empty() {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// Is the run's owner still a live dorker process? A reused pid that is not dorker is not.
pub fn owner_alive(m: &Manifest) -> bool {
    crate::store::pid_alive(m.owner_pid)
        && runtime::cmdline_of(m.owner_pid).is_some_and(|c| c.contains("dorker"))
}

/// Remove the instances of runs whose owner is gone (killed with SIGKILL, or a crash), then
/// their manifests. Returns notes for the user.
pub fn reclaim_stale(store: &Store, sdk: &Sdk) -> Vec<String> {
    let mut notes = Vec::new();
    for m in load_manifests(store) {
        if m.owner_pid == std::process::id() || owner_alive(&m) {
            continue;
        }
        let owned = owned_instances(store, &m.run_id);
        let mut left = Vec::new();
        for inst in &owned {
            if let Err(e) = remove_owned(store, sdk, &inst.id) {
                left.push(format!("{} ({e:#})", inst.id));
            }
        }
        if left.is_empty() {
            remove_run_dir(store, &m.run_id);
            notes.push(format!(
                "removed {} instance(s) left behind by interrupted run {}",
                owned.len(),
                m.run_id
            ));
        } else {
            notes.push(format!(
                "could not remove instances of interrupted run {}: {}",
                m.run_id,
                left.join(", ")
            ));
        }
    }
    notes
}

pub fn owned_instances(store: &Store, run_id: &str) -> Vec<Instance> {
    store
        .load_all()
        .unwrap_or_default()
        .into_iter()
        .filter(|i| i.owner.as_ref().is_some_and(|o| o.run_id == run_id))
        .collect()
}

/// Stop (verified process identity, bounded waits) and delete one run-owned instance, under
/// its operation lock. Idempotent: an instance that is already gone is fine.
pub fn remove_owned(store: &Store, sdk: &Sdk, id: &str) -> Result<()> {
    if !store.instance_dir(id).exists() {
        return Ok(());
    }
    let _op = store.lock_instance(id, OP_TIMEOUT)?;
    let Ok(mut inst) = store.find(id) else {
        return Ok(());
    };
    if inst.id != id || inst.owner.is_none() {
        bail!("refusing to remove {id}: it is not owned by a worker pool");
    }
    runtime::reconcile(store, &mut inst)?;
    if inst.state.expects_process() {
        if inst.state != State::Stopping {
            inst.set_state(State::Stopping)?;
            store.save(&inst)?;
        }
        runtime::kill_instance(sdk, &inst, Duration::from_secs(5));
        if runtime::is_our_process(&inst) {
            bail!(
                "emulator pid {} of {} is still running after SIGKILL",
                inst.pid.unwrap_or(0),
                inst.name
            );
        }
        inst.set_state(State::Stopped)?;
        inst.pid = None;
        store.save(&inst)?;
    }
    store.remove(&inst)
}

// ---------- the pool ----------

pub struct PoolSpec {
    /// A built image (tag or id). It is resolved once; every worker uses that exact id.
    pub image: String,
    pub workers: u32,
    /// Seconds each worker may take to boot.
    pub startup_timeout: u64,
    pub pull: bool,
    pub reserve_mb: u64,
}

/// Progress of a pool, for the caller to present. Never printed on stdout by the pool.
#[derive(Debug)]
pub enum Event<'a> {
    Warning(&'a str),
    Starting {
        worker: u32,
    },
    Ready {
        worker: u32,
        inst: &'a Instance,
        ready_secs: f64,
        restored: bool,
    },
    StartFailed {
        worker: u32,
        error: &'a anyhow::Error,
    },
    Cancelled {
        worker: u32,
    },
    Removed {
        worker: u32,
        instance: &'a str,
    },
}

/// A booted worker, handed to the caller's work function.
pub struct ReadyWorker {
    pub index: u32,
    pub started: Started,
    /// Queue + boot time.
    pub startup_secs: f64,
}

pub enum Outcome<T> {
    /// The work ran; its instance has been removed (see `Pool::cleanup` for failures).
    Completed {
        inst: Instance,
        ready_secs: f64,
        restored: bool,
        value: T,
    },
    /// The worker never became ready. Its instance record (if one was created) is kept until
    /// `Pool::cleanup`, so its `emulator.log` can be collected first.
    StartFailed {
        error: anyhow::Error,
        inst: Option<Instance>,
    },
    /// Cancelled before its work began.
    Cancelled { inst: Option<Instance> },
}

pub struct WorkerResult<T> {
    pub index: u32,
    pub startup_secs: f64,
    pub outcome: Outcome<T>,
}

#[derive(Debug, Default)]
pub struct CleanupReport {
    pub removed: Vec<String>,
    /// Instances that could not be removed: (instance id, error). They still exist.
    pub survivors: Vec<(String, String)>,
}

impl CleanupReport {
    pub fn ok(&self) -> bool {
        self.survivors.is_empty()
    }
}

pub struct Pool {
    pub run_id: String,
    pub image: ImageMeta,
    pub workers: u32,
    pub cost: WorkerCost,
    store: Store,
    sdk: Sdk,
    startup_timeout: u64,
    pull: bool,
    reserve_mb: u64,
    probe: fn(&Store) -> HostSnapshot,
    /// Cancels everything (Ctrl-C, or the caller).
    cancel: Arc<AtomicBool>,
    /// Cancels pending starts only (a worker failed to start).
    stop_starts: Arc<AtomicBool>,
    report: Mutex<CleanupReport>,
    cleaned: bool,
}

impl Pool {
    /// Resolve the image, reclaim stale runs, check capacity for every worker and record the
    /// claim. Nothing is started yet.
    pub fn prepare(
        store: Store,
        sdk: Sdk,
        spec: PoolSpec,
        probe: fn(&Store) -> HostSnapshot,
    ) -> Result<(Pool, Vec<String>)> {
        let cfg = errors::with(errors::CONFIG);
        if spec.workers == 0 {
            return Err(cfg(anyhow::anyhow!("--workers must be at least 1")));
        }
        if spec.image.starts_with("android:") {
            return Err(cfg(anyhow::anyhow!(
                "workers start from a built image (with the app and its test APK installed), not {}\n\nBuild one first: dorker build -t myapp:tests .",
                spec.image
            )));
        }
        let image = images::find(&store, &spec.image).map_err(cfg)?;
        let cost = WorkerCost::for_image(&store, &image);
        let mut notes = reclaim_stale(&store, &sdk);

        let run_id = new_id();
        let manifest = Manifest {
            run_id: run_id.clone(),
            owner_pid: std::process::id(),
            created_at: now_secs(),
            image_id: image.id.clone(),
            workers: spec.workers,
            cost,
            released: vec![],
        };
        {
            let _lock = store.lock_pool().map_err(errors::with(errors::CONFLICT))?;
            let others = live_claims(&store, None);
            let warnings = admit(
                &probe(&store),
                cost.fresh(spec.workers),
                others,
                &cost,
                spec.reserve_mb,
                runtime::min_free_bytes(),
            )
            .map_err(|m| errors::with(errors::CONFLICT)(anyhow::anyhow!(m)))?;
            notes.extend(warnings);
            save_manifest(&store, &manifest)?;
        }
        Ok((
            Pool {
                run_id,
                image,
                workers: spec.workers,
                cost,
                store,
                sdk,
                startup_timeout: spec.startup_timeout,
                pull: spec.pull,
                reserve_mb: spec.reserve_mb,
                probe,
                cancel: Arc::new(AtomicBool::new(false)),
                stop_starts: Arc::new(AtomicBool::new(false)),
                report: Mutex::new(CleanupReport::default()),
                cleaned: false,
            },
            notes,
        ))
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn sdk(&self) -> &Sdk {
        &self.sdk
    }

    /// Cancel everything: pending starts and running work.
    pub fn cancel(&self) {
        self.stop_starts.store(true, Ordering::SeqCst);
        self.cancel.store(true, Ordering::SeqCst);
    }

    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    /// The instance name for a worker: unique per run, well under the 64-character limit.
    pub fn worker_name(&self, worker: u32) -> String {
        format!("run-{}-w{worker}", self.run_id)
    }

    /// Start every worker at once (one thread each), run `work` on each as soon as it is
    /// ready, then remove it. Ctrl-C (or SIGTERM) cancels everything. When a worker fails to
    /// start, the starts still pending are cancelled, but work already running finishes, so
    /// its results are not lost. Blocks until every worker has finished.
    pub fn run<T: Send>(
        &self,
        events: &(dyn Fn(&Event) + Sync),
        work: impl Fn(&ReadyWorker, &dyn Fn() -> bool) -> T + Sync,
    ) -> Vec<WorkerResult<T>> {
        runtime::install_interrupt_handler();
        let done = AtomicBool::new(false);
        std::thread::scope(|s| {
            s.spawn(|| {
                while !done.load(Ordering::SeqCst) {
                    if runtime::interrupted() && !self.cancelled() {
                        self.cancel();
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            });
            let handles: Vec<_> = (0..self.workers)
                .map(|i| {
                    let work = &work;
                    s.spawn(move || self.run_worker(i, events, work))
                })
                .collect();
            let results = handles
                .into_iter()
                .enumerate()
                .map(|(i, h)| {
                    h.join().unwrap_or_else(|_| WorkerResult {
                        index: i as u32,
                        startup_secs: 0.0,
                        outcome: Outcome::StartFailed {
                            error: anyhow::anyhow!("worker {i} panicked"),
                            inst: None,
                        },
                    })
                })
                .collect();
            done.store(true, Ordering::SeqCst);
            results
        })
    }

    fn run_worker<T>(
        &self,
        index: u32,
        events: &(dyn Fn(&Event) + Sync),
        work: &(impl Fn(&ReadyWorker, &dyn Fn() -> bool) -> T + Sync),
    ) -> WorkerResult<T> {
        let t0 = Instant::now();
        let result = |outcome| WorkerResult {
            index,
            startup_secs: t0.elapsed().as_secs_f64(),
            outcome,
        };
        if self.stop_starts.load(Ordering::SeqCst) {
            self.release(index);
            events(&Event::Cancelled { worker: index });
            return result(Outcome::Cancelled { inst: None });
        }
        if let Err(e) = self.recheck(index) {
            self.stop_starts.store(true, Ordering::SeqCst);
            self.release(index);
            events(&Event::StartFailed {
                worker: index,
                error: &e,
            });
            return result(Outcome::StartFailed {
                error: e,
                inst: None,
            });
        }
        events(&Event::Starting { worker: index });
        let args = RunArgs {
            image: self.image.id.clone(),
            name: Some(self.worker_name(index)),
            detach: true,
            rm: false,
            memory: None,
            cpus: None,
            adb_port: None,
            timeout: self.startup_timeout,
            gpu: None,
            json: false,
            pull: self.pull,
            logs: false,
            owner: Some(RunOwner {
                run_id: self.run_id.clone(),
                worker: index,
            }),
            cancel: Some(self.stop_starts.clone()),
        };
        let store = match Store::at(self.store.root()) {
            Ok(s) => s,
            Err(e) => {
                return result(Outcome::StartFailed {
                    error: e,
                    inst: None,
                });
            }
        };
        let started = match commands::start_in(store, self.sdk.clone(), &args, false) {
            Ok(s) => s,
            Err(e) => {
                let inst = self.instance_of(index);
                if self.stop_starts.load(Ordering::SeqCst) {
                    events(&Event::Cancelled { worker: index });
                    return result(Outcome::Cancelled { inst });
                }
                self.stop_starts.store(true, Ordering::SeqCst);
                events(&Event::StartFailed {
                    worker: index,
                    error: &e,
                });
                return result(Outcome::StartFailed { error: e, inst });
            }
        };
        let startup_secs = t0.elapsed().as_secs_f64();
        events(&Event::Ready {
            worker: index,
            inst: &started.inst,
            ready_secs: started.ready_secs,
            restored: started.restored,
        });
        if self.cancelled() {
            let inst = started.inst.clone();
            self.teardown(index, &inst.id, events);
            events(&Event::Cancelled { worker: index });
            return result(Outcome::Cancelled { inst: Some(inst) });
        }
        let ready = ReadyWorker {
            index,
            started,
            startup_secs,
        };
        let value = work(&ready, &|| self.cancelled());
        let ReadyWorker { started, .. } = ready;
        self.teardown(index, &started.inst.id, events);
        WorkerResult {
            index,
            startup_secs,
            outcome: Outcome::Completed {
                inst: started.inst,
                ready_secs: started.ready_secs,
                restored: started.restored,
                value,
            },
        }
    }

    /// Admission recheck right before a worker starts: the host may have changed since the
    /// preflight (other runs, other instances).
    fn recheck(&self, index: u32) -> Result<()> {
        let _lock = self
            .store
            .lock_pool()
            .map_err(errors::with(errors::CONFLICT))?;
        let ours = self.manifest().map_or(Claim::default(), |m| {
            claim_of(&m, &self.store.load_all().unwrap_or_default())
        });
        let others = live_claims(&self.store, Some(&self.run_id));
        let warnings = admit(
            &(self.probe)(&self.store),
            ours,
            others,
            &self.cost,
            self.reserve_mb,
            runtime::min_free_bytes(),
        );
        warnings
            .map(|_| ())
            .map_err(|m| errors::with(errors::CONFLICT)(anyhow::anyhow!("worker {index}: {m}")))
    }

    fn manifest(&self) -> Option<Manifest> {
        let bytes = std::fs::read(manifest_path(&self.store, &self.run_id)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Stop counting a worker's claim.
    fn release(&self, index: u32) {
        let Ok(_lock) = self.store.lock_pool() else {
            return;
        };
        if let Some(mut m) = self.manifest() {
            if !m.released.contains(&index) {
                m.released.push(index);
                let _ = save_manifest(&self.store, &m);
            }
        }
    }

    fn instance_of(&self, index: u32) -> Option<Instance> {
        owned_instances(&self.store, &self.run_id)
            .into_iter()
            .find(|i| i.owner.as_ref().is_some_and(|o| o.worker == index))
    }

    fn teardown(&self, index: u32, id: &str, events: &(dyn Fn(&Event) + Sync)) {
        let r = remove_owned(&self.store, &self.sdk, id);
        let mut report = self.report.lock().unwrap_or_else(|e| e.into_inner());
        match r {
            Ok(()) => {
                report.removed.push(id.to_string());
                drop(report);
                self.release(index);
                events(&Event::Removed {
                    worker: index,
                    instance: id,
                });
            }
            Err(e) => report.survivors.push((id.to_string(), format!("{e:#}"))),
        }
    }

    /// Remove every instance this run still owns (partial starts, failed boots, anything a
    /// worker could not remove), in parallel, then the run's manifest. Idempotent. The report
    /// includes failures from earlier per-worker removals; survivors are still running or
    /// still on disk, and keep the manifest so a later run can reclaim them.
    pub fn cleanup(&mut self) -> CleanupReport {
        self.stop_starts.store(true, Ordering::SeqCst);
        let mut report =
            std::mem::take(&mut *self.report.lock().unwrap_or_else(|e| e.into_inner()));
        if self.cleaned {
            return report;
        }
        let owned = owned_instances(&self.store, &self.run_id);
        let (store, sdk) = (&self.store, &self.sdk);
        let results: Vec<(String, Result<()>)> = std::thread::scope(|s| {
            let handles: Vec<_> = owned
                .iter()
                .map(|i| {
                    let id = i.id.clone();
                    s.spawn(move || {
                        let r = remove_owned(store, sdk, &id);
                        (id, r)
                    })
                })
                .collect();
            handles.into_iter().filter_map(|h| h.join().ok()).collect()
        });
        // An earlier failure that a retry here fixed is no longer a survivor.
        let gone = |id: &str| !self.store.instance_dir(id).exists();
        report.survivors.retain(|(id, _)| !gone(id));
        for (id, r) in results {
            match r {
                Ok(()) => report.removed.push(id),
                Err(e) => {
                    if !report.survivors.iter().any(|(s, _)| *s == id) {
                        report.survivors.push((id, format!("{e:#}")));
                    }
                }
            }
        }
        if report.survivors.is_empty() {
            remove_run_dir(&self.store, &self.run_id);
        }
        self.cleaned = true;
        report
    }
}

impl Drop for Pool {
    /// A fallback only: callers call `cleanup` and report its result.
    fn drop(&mut self) {
        if !self.cleaned {
            let r = self.cleanup();
            for (id, e) in r.survivors {
                eprintln!("warning: could not remove worker instance {id}: {e}");
            }
        }
    }
}

/// Claims of every live run except `except`.
fn live_claims(store: &Store, except: Option<&str>) -> Claim {
    let instances = store.load_all().unwrap_or_default();
    load_manifests(store)
        .iter()
        .filter(|m| Some(m.run_id.as_str()) != except)
        .filter(|m| m.owner_pid == std::process::id() || owner_alive(m))
        .map(|m| claim_of(m, &instances))
        .fold(Claim::default(), |a, b| a + b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::FakeSdk;
    use std::sync::atomic::AtomicU32;

    const GB: u64 = 1024 * MIB;

    fn cost() -> WorkerCost {
        WorkerCost {
            guest_memory_mb: 2048,
            host_memory_mb: estimate_host_mb(2048),
            cpus: 2,
            disk_bytes: GB,
        }
    }

    fn host(mem: Option<u64>, disk: Option<u64>, ports: usize) -> HostSnapshot {
        HostSnapshot {
            available_memory_mb: mem,
            free_disk_bytes: disk,
            free_port_pairs: ports,
            host_cpus: 64,
            running_cpus: 0,
        }
    }

    #[test]
    fn the_estimate_is_above_the_guest_ram() {
        assert_eq!(estimate_host_mb(2048), 3584);
        assert!(estimate_host_mb(1024) > 1024);
    }

    #[test]
    fn admits_when_everything_fits_with_the_reserve() {
        let c = cost();
        let need = 4 * c.host_memory_mb + 2048;
        let w = admit(
            &host(Some(need), Some(100 * GB), 60),
            c.fresh(4),
            Claim::default(),
            &c,
            2048,
            2 * GB,
        )
        .unwrap();
        assert!(w.is_empty(), "{w:?}");
        // One MB less and the reserve would be eaten into.
        assert!(
            admit(
                &host(Some(need - 1), Some(100 * GB), 60),
                c.fresh(4),
                Claim::default(),
                &c,
                2048,
                2 * GB
            )
            .is_err()
        );
    }

    #[test]
    fn low_memory_says_how_many_workers_fit() {
        let c = cost();
        let err = admit(
            &host(Some(2048 + 2 * c.host_memory_mb + 100), Some(100 * GB), 60),
            c.fresh(4),
            Claim::default(),
            &c,
            2048,
            2 * GB,
        )
        .unwrap_err();
        assert!(err.contains("memory: 4 worker(s)"), "{err}");
        assert!(err.contains("--workers 2"), "{err}");
        assert!(err.contains("estimate"), "{err}");
    }

    #[test]
    fn low_disk_and_port_exhaustion_are_capacity_errors() {
        let c = cost();
        let err = admit(
            &host(Some(1 << 30), Some(2 * GB + 3 * GB / 2), 60),
            c.fresh(3),
            Claim::default(),
            &c,
            0,
            2 * GB,
        )
        .unwrap_err();
        assert!(err.contains("disk: 3 worker(s)"), "{err}");
        assert!(err.contains("--workers 1"), "{err}");
        let err = admit(
            &host(Some(1 << 30), Some(100 * GB), 2),
            c.fresh(3),
            Claim::default(),
            &c,
            0,
            0,
        )
        .unwrap_err();
        assert!(err.contains("ports: 3 worker(s)"), "{err}");
        assert!(err.contains("--workers 2"), "{err}");
    }

    #[test]
    fn missing_metrics_warn_and_never_invent_numbers() {
        let c = cost();
        let w = admit(
            &host(None, None, 60),
            c.fresh(8),
            Claim::default(),
            &c,
            2048,
            0,
        )
        .unwrap();
        assert_eq!(w.len(), 2, "{w:?}");
        assert!(w[0].contains("could not read the host's available memory"));
        assert!(w[0].contains("3584 MB each"), "{}", w[0]);
        assert!(w[1].contains("free disk space"));
        // Ports are always known, so they still bound admission.
        assert!(admit(&host(None, None, 3), c.fresh(8), Claim::default(), &c, 0, 0).is_err());
    }

    #[test]
    fn other_runs_claims_count_against_the_same_capacity() {
        let c = cost();
        let h = host(Some(2048 + 4 * c.host_memory_mb), Some(100 * GB), 4);
        assert!(admit(&h, c.fresh(4), Claim::default(), &c, 2048, 0).is_ok());
        let err = admit(&h, c.fresh(4), c.fresh(2), &c, 2048, 0).unwrap_err();
        assert!(err.contains("claimed by other runs"), "{err}");
        assert!(err.contains("--workers 2"), "{err}");
    }

    #[test]
    fn cpu_oversubscription_is_a_warning() {
        let c = cost();
        let mut h = host(Some(1 << 30), Some(100 * GB), 60);
        h.host_cpus = 4;
        h.running_cpus = 2;
        let w = admit(&h, c.fresh(2), Claim::default(), &c, 0, 0).unwrap();
        assert!(
            w.iter().any(|w| w.contains("6 vCPUs on 4 host cores")),
            "{w:?}"
        );
    }

    fn owned(run: &str, worker: u32, state: State) -> Instance {
        let mut i: Instance = serde_json::from_value(serde_json::json!({
            "id": format!("{run}{worker}"), "name": format!("{run}-{worker}"), "image": "x",
            "api": 35, "flavour": "google_apis", "abi": "x86_64", "memory_mb": 2048, "cpus": 2,
            "disk_mb": 4096, "headless": true, "auto_remove": false, "console_port": 5554,
            "pid": null, "state": "CREATED", "created_at": 1,
        }))
        .unwrap();
        i.state = state;
        i.owner = Some(RunOwner {
            run_id: run.into(),
            worker,
        });
        i
    }

    fn manifest(run: &str, workers: u32) -> Manifest {
        Manifest {
            run_id: run.into(),
            owner_pid: std::process::id(),
            created_at: 1,
            image_id: "img".into(),
            workers,
            cost: cost(),
            released: vec![],
        }
    }

    #[test]
    fn claims_shrink_as_workers_boot_and_finish() {
        let mut m = manifest("r", 5);
        m.released = vec![4];
        let insts = [
            owned("r", 1, State::Booting),
            owned("r", 2, State::Ready),
            owned("r", 3, State::Failed),
            owned("other", 0, State::Booting),
        ];
        let c = claim_of(&m, &insts);
        // Worker 0 has no instance (everything), 1 boots (memory), 2-4 claim nothing more.
        assert_eq!(c.workers, 2);
        assert_eq!(c.memory_mb, 2 * cost().host_memory_mb);
        assert_eq!(c.disk_bytes, GB);
        assert_eq!(c.ports, 1);
        assert_eq!(c.cpus, 2);
    }

    #[test]
    fn host_memory_parsing() {
        let meminfo = "MemTotal:       32856000 kB\nMemFree:          822000 kB\nMemAvailable:   14560000 kB\n";
        assert_eq!(parse_meminfo(meminfo), Some(14218));
        assert_eq!(parse_meminfo("MemTotal: 1 kB\n"), None);
        let vm = "Mach Virtual Memory Statistics: (page size of 16384 bytes)\nPages free:                               10000.\nPages active:                            500000.\nPages inactive:                           50000.\nPages speculative:                         2000.\nPages purgeable:                           1000.\n";
        assert_eq!(
            parse_vm_stat(vm),
            Some((10000 + 50000 + 2000 + 1000) * 16384 / MIB)
        );
        assert_eq!(parse_vm_stat("garbage"), None);
    }

    fn roomy(_: &Store) -> HostSnapshot {
        HostSnapshot {
            available_memory_mb: Some(1 << 30),
            free_disk_bytes: Some(1 << 50),
            free_port_pairs: 60,
            host_cpus: 64,
            running_cpus: 0,
        }
    }

    /// Room for exactly two 1 GB workers (2048 MB each on the host) and no reserve.
    fn two_workers(s: &Store) -> HostSnapshot {
        HostSnapshot {
            available_memory_mb: Some(2 * estimate_host_mb(1024)),
            ..roomy(s)
        }
    }

    fn spec(image: &str, workers: u32) -> PoolSpec {
        PoolSpec {
            image: image.into(),
            workers,
            startup_timeout: 20,
            pull: false,
            reserve_mb: 0,
        }
    }

    fn pool(f: &FakeSdk, image: &str, workers: u32, probe: fn(&Store) -> HostSnapshot) -> Pool {
        Pool::prepare(f.store(), f.sdk(), spec(image, workers), probe)
            .unwrap()
            .0
    }

    fn quiet(_: &Event) {}

    fn wait_for(what: &str, secs: u64, cond: impl Fn() -> bool) -> bool {
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(secs) {
            if cond() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        eprintln!("gave up waiting for {what}");
        cond()
    }

    #[test]
    fn invalid_specs_are_configuration_errors() {
        let f = FakeSdk::new();
        f.image("t:1", 1024);
        let err = Pool::prepare(f.store(), f.sdk(), spec("t:1", 0), roomy)
            .err()
            .unwrap();
        assert_eq!(errors::code_of(&err), errors::CONFIG);
        let err = Pool::prepare(f.store(), f.sdk(), spec("android:35", 2), roomy)
            .err()
            .unwrap();
        assert_eq!(errors::code_of(&err), errors::CONFIG);
        assert!(format!("{err:#}").contains("built image"), "{err:#}");
        let err = Pool::prepare(f.store(), f.sdk(), spec("nope:1", 2), roomy)
            .err()
            .unwrap();
        assert_eq!(errors::code_of(&err), errors::CONFIG);
    }

    #[test]
    fn concurrent_pools_cannot_both_claim_the_last_capacity() {
        let f = FakeSdk::new();
        f.image("t:1", 1024);
        let (first, _) = Pool::prepare(f.store(), f.sdk(), spec("t:1", 1), two_workers).unwrap();
        let err = Pool::prepare(f.store(), f.sdk(), spec("t:1", 2), two_workers)
            .err()
            .unwrap();
        assert_eq!(errors::code_of(&err), errors::CONFLICT);
        assert!(format!("{err:#}").contains("--workers 1"), "{err:#}");
        // Nothing was created for the refused run, and only one claim is on disk.
        assert!(f.store().load_all().unwrap().is_empty());
        assert_eq!(load_manifests(&f.store()).len(), 1);
        drop(first);
        assert!(
            load_manifests(&f.store()).is_empty(),
            "the claim is released"
        );
        assert!(Pool::prepare(f.store(), f.sdk(), spec("t:1", 2), two_workers).is_ok());
    }

    #[test]
    fn workers_boot_together_on_distinct_serials_and_private_avds_then_vanish() {
        let f = FakeSdk::new();
        let image = f.image("t:1", 1024);
        let mut p = pool(&f, "t:1", 3, roomy);
        let ready = AtomicU32::new(0);
        let results = p.run(&quiet, |w, _| {
            ready.fetch_add(1, Ordering::SeqCst);
            let together = wait_for("all workers", 10, || ready.load(Ordering::SeqCst) == 3);
            let avd = std::fs::read_to_string(
                f.root()
                    .join(format!("pids/{}.avdhome", w.started.inst.console_port)),
            )
            .unwrap();
            (together, w.started.inst.clone(), avd.trim().to_string())
        });
        assert_eq!(results.len(), 3);
        let mut serials = Vec::new();
        for r in &results {
            let Outcome::Completed { inst, value, .. } = &r.outcome else {
                panic!("worker {} did not complete", r.index);
            };
            let (together, seen, avd) = value;
            assert!(together, "all three were ready at the same time");
            assert_eq!(inst.name, p.worker_name(r.index));
            assert_eq!(seen.image, image.id, "the tag was resolved once, to the id");
            assert_eq!(
                inst.owner,
                Some(RunOwner {
                    run_id: p.run_id.clone(),
                    worker: r.index
                })
            );
            assert_eq!(
                *avd,
                p.store()
                    .instance_dir(&inst.id)
                    .join("avd")
                    .display()
                    .to_string()
            );
            serials.push(inst.serial());
        }
        serials.sort();
        serials.dedup();
        assert_eq!(serials.len(), 3, "{serials:?}");
        let report = p.cleanup();
        assert!(report.ok(), "{report:?}");
        assert_eq!(report.removed.len(), 3);
        assert!(f.store().load_all().unwrap().is_empty());
        assert!(f.live_emulators().is_empty());
        assert!(load_manifests(&f.store()).is_empty());
    }

    #[test]
    fn a_failed_start_cancels_pending_starts_and_everything_owned_is_removed() {
        let f = FakeSdk::new();
        f.image("t:1", 1024);
        // The first emulator to spawn crashes after 2 s; the other is ready long before.
        std::fs::write(f.root().join("crash-next"), "2").unwrap();
        let mut p = pool(&f, "t:1", 2, roomy);
        let results = p.run(&quiet, |_, cancelled| {
            wait_for("cancellation", 4, cancelled);
            cancelled()
        });
        let failed: Vec<_> = results
            .iter()
            .filter_map(|r| match &r.outcome {
                Outcome::StartFailed { error, inst } => Some((error, inst.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(failed.len(), 1);
        let (error, inst) = &failed[0];
        assert!(
            format!("{error:#}").contains("exited during startup"),
            "{error:#}"
        );
        let inst = inst.clone().expect("the partial instance is reported");
        assert_eq!(inst.state, State::Failed);
        let log = p.store().instance_dir(&inst.id).join("emulator.log");
        assert!(
            std::fs::read_to_string(&log)
                .unwrap()
                .contains("crashed: boom"),
            "diagnostics are still there before cleanup"
        );
        let completed = results
            .iter()
            .filter(|r| matches!(r.outcome, Outcome::Completed { .. }))
            .count();
        assert_eq!(completed, 1, "work that already runs is not cancelled");
        let report = p.cleanup();
        assert!(report.ok(), "{report:?}");
        assert!(report.removed.contains(&inst.id));
        assert!(f.store().load_all().unwrap().is_empty());
        assert!(f.live_emulators().is_empty());
    }

    #[test]
    fn cancelling_during_boot_or_work_removes_only_owned_instances() {
        let f = FakeSdk::new();
        let image = f.image("t:1", 1024);
        let bystander = commands::start_in(
            f.store(),
            f.sdk(),
            &RunArgs {
                image: image.id.clone(),
                name: Some("bystander".into()),
                detach: true,
                rm: false,
                memory: None,
                cpus: None,
                adb_port: None,
                timeout: 20,
                gpu: None,
                json: false,
                pull: false,
                logs: false,
                owner: None,
                cancel: None,
            },
            false,
        )
        .unwrap()
        .inst;

        // During boot: nothing ever becomes ready.
        std::fs::write(f.root().join("not-booting"), "").unwrap();
        let mut p = pool(&f, "t:1", 2, roomy);
        let results = std::thread::scope(|s| {
            s.spawn(|| {
                wait_for("both workers to spawn", 10, || {
                    owned_instances(p.store(), &p.run_id)
                        .iter()
                        .all(|i| i.pid.is_some())
                        && owned_instances(p.store(), &p.run_id).len() == 2
                });
                p.cancel();
            });
            p.run(&quiet, |_, _| ())
        });
        assert!(
            results
                .iter()
                .all(|r| matches!(r.outcome, Outcome::Cancelled { .. })),
            "every start was cancelled"
        );
        assert!(p.cleanup().ok());
        std::fs::remove_file(f.root().join("not-booting")).unwrap();

        // During work.
        let mut p = pool(&f, "t:1", 2, roomy);
        let ready = AtomicU32::new(0);
        let results = std::thread::scope(|s| {
            s.spawn(|| {
                wait_for("both workers", 10, || ready.load(Ordering::SeqCst) == 2);
                p.cancel();
            });
            p.run(&quiet, |_, cancelled| {
                ready.fetch_add(1, Ordering::SeqCst);
                wait_for("cancellation", 10, cancelled)
            })
        });
        for r in &results {
            assert!(
                matches!(r.outcome, Outcome::Completed { value: true, .. }),
                "work saw the cancellation"
            );
        }
        assert!(p.cleanup().ok());

        let left = f.store().load_all().unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].id, bystander.id);
        let mut b = left[0].clone();
        runtime::reconcile(&f.store(), &mut b).unwrap();
        assert_eq!(b.state, State::Ready, "an unrelated instance keeps running");
        assert_eq!(f.live_emulators().len(), 1);
        commands::stop_instance(&f.sdk(), &f.store(), &mut b).unwrap();
    }

    #[test]
    fn two_pools_never_share_names_ports_or_instances() {
        let f = FakeSdk::new();
        f.image("t:1", 1024);
        let mut a = pool(&f, "t:1", 2, roomy);
        let mut b = pool(&f, "t:1", 2, roomy);
        assert_ne!(a.run_id, b.run_id);
        let ready = AtomicU32::new(0);
        let a_done = AtomicBool::new(false);
        let (ra, rb) = std::thread::scope(|s| {
            let ha = s.spawn(|| {
                let r = a.run(&quiet, |w, _| {
                    ready.fetch_add(1, Ordering::SeqCst);
                    wait_for("all four", 10, || ready.load(Ordering::SeqCst) == 4);
                    w.started.inst.clone()
                });
                a_done.store(true, Ordering::SeqCst);
                r
            });
            let rb = b.run(&quiet, |w, _| {
                ready.fetch_add(1, Ordering::SeqCst);
                wait_for("pool a to finish", 20, || a_done.load(Ordering::SeqCst));
                // Pool a removed its own workers; this one is untouched.
                let mut me = f.store().find(&w.started.inst.id).unwrap();
                runtime::reconcile(&f.store(), &mut me).unwrap();
                (w.started.inst.clone(), me.state)
            });
            (ha.join().unwrap(), rb)
        });
        let mut names = Vec::new();
        let mut serials = Vec::new();
        for r in &ra {
            let Outcome::Completed { inst, .. } = &r.outcome else {
                panic!("a{} failed", r.index)
            };
            names.push(inst.name.clone());
            serials.push(inst.serial());
        }
        for r in &rb {
            let Outcome::Completed { inst, value, .. } = &r.outcome else {
                panic!("b{} failed", r.index)
            };
            assert_eq!(value.1, State::Ready);
            names.push(inst.name.clone());
            serials.push(inst.serial());
        }
        names.sort();
        names.dedup();
        serials.sort();
        serials.dedup();
        assert_eq!((names.len(), serials.len()), (4, 4));
        assert!(a.cleanup().ok() && b.cleanup().ok());
        assert!(f.store().load_all().unwrap().is_empty());
    }

    #[test]
    fn stale_runs_are_reclaimed_but_live_and_unowned_instances_are_kept() {
        let f = FakeSdk::new();
        let store = f.store();
        let mut dead = std::process::Command::new("true").spawn().unwrap();
        let dead_pid = dead.id();
        dead.wait().unwrap();
        // A live process that is not dorker: a reused pid must not keep a run alive.
        let mut other = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();

        let mut stale = manifest("stale", 1);
        stale.owner_pid = dead_pid;
        let mut reused = manifest("reused", 1);
        reused.owner_pid = other.id();
        let live = manifest("live", 1);
        for m in [&stale, &reused, &live] {
            save_manifest(&store, m).unwrap();
        }
        store.save(&owned("stale", 0, State::Stopped)).unwrap();
        store.save(&owned("reused", 0, State::Failed)).unwrap();
        store.save(&owned("live", 0, State::Stopped)).unwrap();
        let mut unowned = owned("mine", 0, State::Stopped);
        unowned.owner = None;
        store.save(&unowned).unwrap();

        let notes = reclaim_stale(&store, &f.sdk());
        assert_eq!(notes.len(), 2, "{notes:?}");
        let mut left: Vec<String> = store
            .load_all()
            .unwrap()
            .into_iter()
            .map(|i| i.id)
            .collect();
        left.sort();
        assert_eq!(left, vec!["live0".to_string(), "mine0".to_string()]);
        let runs: Vec<String> = load_manifests(&store)
            .into_iter()
            .map(|m| m.run_id)
            .collect();
        assert_eq!(runs, vec!["live".to_string()]);
        // Removing an instance that no pool owns is refused outright.
        assert!(remove_owned(&store, &f.sdk(), "mine0").is_err());
        let _ = other.kill();
        let _ = other.wait();
    }

    #[test]
    fn cleanup_failures_name_the_survivors_and_keep_the_claim() {
        use std::os::unix::fs::PermissionsExt;
        let f = FakeSdk::new();
        f.image("t:1", 1024);
        let mut p = pool(&f, "t:1", 1, roomy);
        let stuck = std::sync::Mutex::new(None);
        let results = p.run(&quiet, |w, _| {
            // A folder whose contents cannot be deleted makes removal fail.
            let dir = p.store().instance_dir(&w.started.inst.id).join("stuck");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("file"), "x").unwrap();
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
            *stuck.lock().unwrap() = Some(dir);
        });
        let dir = stuck.lock().unwrap().clone().unwrap();
        let root_can_delete = std::fs::write(dir.join("probe"), "").is_ok();
        assert!(matches!(results[0].outcome, Outcome::Completed { .. }));
        let report = p.cleanup();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        if !root_can_delete {
            assert!(!report.ok());
            assert_eq!(report.survivors.len(), 1, "{report:?}");
            assert!(report.survivors[0].1.contains("removing"), "{report:?}");
            assert_eq!(
                load_manifests(p.store()).len(),
                1,
                "kept for a later reclaim"
            );
            assert!(
                f.live_emulators().is_empty(),
                "the emulator itself was stopped"
            );
            remove_owned(p.store(), p.sdk(), &report.survivors[0].0).unwrap();
        }
        assert!(f.store().load_all().unwrap().is_empty());
    }
}
