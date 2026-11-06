use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Explicit instance lifecycle (PRD §10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum State {
    Created,
    Starting,
    Booting,
    Ready,
    Stopping,
    Stopped,
    Failed,
    Crashed,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Created => "CREATED",
            State::Starting => "STARTING",
            State::Booting => "BOOTING",
            State::Ready => "READY",
            State::Stopping => "STOPPING",
            State::Stopped => "STOPPED",
            State::Failed => "FAILED",
            State::Crashed => "CRASHED",
        }
    }

    /// States in which an emulator process is expected to exist.
    pub fn expects_process(self) -> bool {
        matches!(
            self,
            State::Starting | State::Booting | State::Ready | State::Stopping
        )
    }

    /// Whether `self -> next` is a legal lifecycle transition.
    pub fn can_become(self, next: State) -> bool {
        use State::*;
        matches!(
            (self, next),
            (Created, Starting)
                | (Starting, Booting)
                | (Booting, Ready)
                | (Starting | Booting | Ready, Stopping)
                | (Stopping, Stopped)
                | (Created | Starting | Booting, Failed)
                | (Starting | Booting | Ready | Stopping, Crashed)
                // A stopped, failed or crashed instance can be started again.
                | (Stopped | Failed | Crashed, Starting)
                // Idempotent updates.
                | (Stopped, Stopped)
                | (Failed, Failed)
                | (Crashed, Crashed)
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Instance {
    pub id: String,
    pub name: String,
    pub image: String,
    pub api: u32,
    pub flavour: String,
    pub abi: String,
    pub memory_mb: u64,
    pub cpus: u32,
    pub disk_mb: u64,
    pub headless: bool,
    pub auto_remove: bool,
    pub console_port: u16,
    pub pid: Option<u32>,
    pub state: State,
    pub created_at: u64,
    #[serde(default = "default_device")]
    pub device: String,
    #[serde(default = "default_width")]
    pub width: u32,
    #[serde(default = "default_height")]
    pub height: u32,
    #[serde(default = "default_gpu")]
    pub gpu: String,
    /// Image this instance was cloned from (`None` for base `android:<api>` runs).
    #[serde(default)]
    pub image_id: Option<String>,
    /// Snapshot to restore at start, if the image has one.
    #[serde(default)]
    pub snapshot: Option<String>,
    /// The worker pool run that owns this instance, if any. Saved before the emulator is
    /// spawned, so a pool can find and remove its partially started instances.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<RunOwner>,
}

/// Which pool run (and which worker in it) created an instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunOwner {
    pub run_id: String,
    pub worker: u32,
}

fn default_gpu() -> String {
    "swiftshader_indirect".into()
}
fn default_device() -> String {
    "pixel_7".into()
}
fn default_width() -> u32 {
    1080
}
fn default_height() -> u32 {
    2400
}

impl Instance {
    /// adb serial of the emulator (`emulator-<console port>`).
    pub fn serial(&self) -> String {
        format!("emulator-{}", self.console_port)
    }

    pub fn adb_port(&self) -> u16 {
        self.console_port + 1
    }

    pub fn set_state(&mut self, next: State) -> Result<()> {
        if !self.state.can_become(next) {
            bail!(
                "invalid state transition for instance {}: {} -> {}",
                self.name,
                self.state.as_str(),
                next.as_str()
            );
        }
        crate::log::vlog!(
            "instance {} ({}): {} -> {}",
            self.name,
            &self.id[..self.id.len().min(12)],
            self.state.as_str(),
            next.as_str()
        );
        self.state = next;
        Ok(())
    }
}

/// Where the store lives: `$DORKER_HOME`, else `home` in the config file, else `~/.dorker`.
pub fn default_root() -> Result<PathBuf> {
    if let Some(v) = std::env::var_os("DORKER_HOME").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(v));
    }
    if let Some(h) = crate::config::get()?.home.clone() {
        return Ok(h);
    }
    Ok(crate::sdk::dirs_home()
        .context("HOME is not set")?
        .join(".dorker"))
}

/// On-disk store rooted at `~/.dorker` (or `$DORKER_HOME`).
///
/// ```text
/// <root>/instances/<id>/instance.json   metadata (atomic writes)
/// <root>/instances/<id>/avd/            private ANDROID_AVD_HOME for this instance
/// <root>/instances/<id>/emulator.log
/// <root>/runs/<run-id>/manifest.json    a worker pool's owner and resource claim
/// <root>/locks/create.lock              serialises name/port allocation
/// <root>/locks/pool.lock                serialises worker pool admission
/// ```
pub struct Store {
    root: PathBuf,
}

#[derive(Debug)]
pub struct LockGuard(PathBuf);

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

impl Store {
    pub fn open() -> Result<Self> {
        Self::at(default_root()?)
    }

    pub fn at(root: PathBuf) -> Result<Self> {
        for sub in ["instances", "images", "locks", "runs"] {
            fs::create_dir_all(root.join(sub))
                .with_context(|| format!("creating {}", root.join(sub).display()))?;
        }
        Ok(Self { root })
    }

    pub fn root(&self) -> PathBuf {
        self.root.clone()
    }

    pub fn instances_dir(&self) -> PathBuf {
        self.root.join("instances")
    }

    pub fn images_dir(&self) -> PathBuf {
        self.root.join("images")
    }

    pub fn runs_dir(&self) -> PathBuf {
        self.root.join("runs")
    }

    pub fn instance_dir(&self, id: &str) -> PathBuf {
        self.instances_dir().join(id)
    }

    pub fn save(&self, inst: &Instance) -> Result<()> {
        let dir = self.instance_dir(&inst.id);
        fs::create_dir_all(&dir)?;
        let tmp = dir.join("instance.json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(inst)?)?;
        fs::rename(&tmp, dir.join("instance.json"))?; // atomic on the same filesystem
        Ok(())
    }

    pub fn load_all(&self) -> Result<Vec<Instance>> {
        let mut out = Vec::new();
        for entry in fs::read_dir(self.instances_dir())? {
            let path = entry?.path().join("instance.json");
            let Ok(bytes) = fs::read(&path) else { continue };
            match serde_json::from_slice::<Instance>(&bytes) {
                Ok(i) => out.push(i),
                Err(e) => eprintln!("warning: skipping unreadable {}: {e}", path.display()),
            }
        }
        out.sort_by_key(|i| i.created_at);
        Ok(out)
    }

    /// Find by exact name, exact id, or unambiguous id prefix.
    pub fn find(&self, key: &str) -> Result<Instance> {
        let all = self.load_all()?;
        if let Some(i) = all.iter().find(|i| i.name == key || i.id == key) {
            return Ok(i.clone());
        }
        let by_prefix: Vec<&Instance> = all.iter().filter(|i| i.id.starts_with(key)).collect();
        match by_prefix.as_slice() {
            [one] => Ok((*one).clone()),
            [] => bail!("no such instance: {key}\n\nRun `dorker ps -a` to list instances."),
            _ => bail!("ambiguous instance id prefix {key:?}; use the full id or name"),
        }
    }

    /// Delete an instance directory. Refuses anything outside `instances/`.
    pub fn remove(&self, inst: &Instance) -> Result<()> {
        let dir = self.instance_dir(&inst.id);
        ensure_within(&self.instances_dir(), &dir)?;
        if dir.exists() {
            fs::remove_dir_all(&dir).with_context(|| format!("removing {}", dir.display()))?;
        }
        Ok(())
    }

    /// Serialise mutating operations (stop, rm, start) on one instance. The lock
    /// file records the holder's pid, so a lock left by a dead process is broken.
    pub fn lock_instance(&self, id: &str, timeout: std::time::Duration) -> Result<LockGuard> {
        let path = self.instance_dir(id).join("op.lock");
        let start = std::time::Instant::now();
        loop {
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut f) => {
                    use std::io::Write;
                    let _ = write!(f, "{}", std::process::id());
                    return Ok(LockGuard(path));
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let holder: Option<u32> = fs::read_to_string(&path)
                        .ok()
                        .and_then(|s| s.trim().parse().ok());
                    if holder.is_some_and(|p| !pid_alive(p)) {
                        let _ = fs::remove_file(&path);
                        continue;
                    }
                    if start.elapsed() > timeout {
                        return Err(crate::errors::with(crate::errors::CONFLICT)(
                            anyhow::anyhow!(
                                "instance is busy (another dorker command, pid {}, is operating on it)",
                                holder.map_or("?".into(), |p| p.to_string())
                            ),
                        ));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    bail!("no such instance directory for {id}")
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// Take the store-wide creation lock (name + port allocation).
    pub fn lock_create(&self) -> Result<LockGuard> {
        self.lock_named("create.lock", LOCK_WAIT, &|| false)
    }

    /// The creation lock for a pool worker, which may queue behind other workers' clones
    /// (minutes without reflinks): waits up to `wait`, and gives up when `cancelled`.
    pub fn lock_create_queued(
        &self,
        wait: std::time::Duration,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<LockGuard> {
        self.lock_named("create.lock", wait, cancelled)
    }

    /// Take the store-wide pool lock, held briefly while a worker pool reads and updates
    /// resource claims, so two pools cannot both claim the same capacity.
    pub fn lock_pool(&self) -> Result<LockGuard> {
        self.lock_named("pool.lock", LOCK_WAIT, &|| false)
    }

    fn lock_named(
        &self,
        file: &str,
        wait: std::time::Duration,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<LockGuard> {
        let path = self.root.join("locks").join(file);
        let start = std::time::Instant::now();
        loop {
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut f) => {
                    use std::io::Write;
                    let _ = write!(f, "{}", std::process::id());
                    return Ok(LockGuard(path));
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    // Break locks left behind by a dead process.
                    if lock_is_stale(&path) {
                        let _ = fs::remove_file(&path);
                        continue;
                    }
                    if cancelled() {
                        bail!("cancelled while waiting for {}", path.display());
                    }
                    if start.elapsed() > wait {
                        bail!("timed out waiting for {}", path.display());
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
}

const LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

pub(crate) fn pid_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// A store lock records its holder's pid; it is stale once that process is gone (or the pid
/// now belongs to something other than dorker). A lock without a pid (being written right
/// now, or from an older dorker) is stale after 30s.
fn lock_is_stale(path: &Path) -> bool {
    let holder: Option<u32> = fs::read_to_string(path)
        .ok()
        .and_then(|s| s.trim().parse().ok());
    if let Some(pid) = holder {
        return !(pid_alive(pid)
            && crate::runtime::cmdline_of(pid).is_some_and(|c| c.contains("dorker")));
    }
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age.as_secs() > 30)
}

fn ensure_within(parent: &Path, child: &Path) -> Result<()> {
    if child.parent() != Some(parent) || child.file_name().is_none() {
        bail!(
            "refusing to touch {} (outside {})",
            child.display(),
            parent.display()
        );
    }
    Ok(())
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Short unique id (12 hex chars) from the clock, pid and a counter.
pub fn new_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0) as u64;
    let mut x = nanos ^ ((std::process::id() as u64) << 32) ^ N.fetch_add(1, Ordering::Relaxed);
    // xorshift mix
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    format!("{:012x}", x & 0xffff_ffff_ffff)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Dir;

    fn sample(id: &str, name: &str) -> Instance {
        Instance {
            id: id.into(),
            name: name.into(),
            image: "android:35".into(),
            api: 35,
            flavour: "google_apis".into(),
            abi: "arm64-v8a".into(),
            memory_mb: 2048,
            cpus: 2,
            disk_mb: 4096,
            headless: true,
            auto_remove: false,
            console_port: 5554,
            pid: None,
            state: State::Created,
            created_at: 1,
            device: "pixel_7".into(),
            width: 1080,
            height: 2400,
            gpu: "swiftshader_indirect".into(),
            image_id: None,
            snapshot: None,
            owner: None,
        }
    }

    #[test]
    fn lifecycle_transitions() {
        use State::*;
        assert!(Created.can_become(Starting));
        assert!(Starting.can_become(Booting));
        assert!(Booting.can_become(Ready));
        assert!(Ready.can_become(Stopping));
        assert!(Stopping.can_become(Stopped));
        assert!(Ready.can_become(Crashed));
        assert!(Stopped.can_become(Starting));
        assert!(!Created.can_become(Ready));
        assert!(!Stopped.can_become(Ready));
        assert!(!Ready.can_become(Booting));
        let mut i = sample("a", "a");
        assert!(i.set_state(State::Ready).is_err());
        assert!(i.set_state(State::Starting).is_ok());
    }

    #[test]
    fn roundtrip_and_find() {
        let d = Dir::new();
        let store = Store::at(d.path().to_path_buf()).unwrap();
        store.save(&sample("abc123", "one")).unwrap();
        store.save(&sample("abd456", "two")).unwrap();
        assert_eq!(store.find("one").unwrap().id, "abc123");
        assert_eq!(store.find("abd").unwrap().name, "two");
        assert!(
            store
                .find("ab")
                .unwrap_err()
                .to_string()
                .contains("ambiguous")
        );
        assert!(store.find("zzz").is_err());
        assert_eq!(store.load_all().unwrap().len(), 2);
        assert_eq!(store.find("one").unwrap().serial(), "emulator-5554");
    }

    #[test]
    fn remove_only_deletes_instance_dir() {
        let d = Dir::new();
        let store = Store::at(d.path().to_path_buf()).unwrap();
        let i = sample("abc123", "one");
        store.save(&i).unwrap();
        store.remove(&i).unwrap();
        assert!(store.find("one").is_err());
        assert!(d.path().join("instances").is_dir());
        // Path traversal through a crafted id is refused.
        let mut evil = sample("../locks", "evil");
        evil.id = "../locks".into();
        assert!(store.remove(&evil).is_err());
        assert!(d.path().join("locks").is_dir());
    }

    #[test]
    fn create_lock_is_exclusive() {
        let d = Dir::new();
        let store = Store::at(d.path().to_path_buf()).unwrap();
        let g = store.lock_create().unwrap();
        let lock = d.path().join("locks/create.lock");
        assert_eq!(
            fs::read_to_string(&lock).unwrap(),
            std::process::id().to_string()
        );
        drop(g);
        assert!(!lock.exists());
    }

    #[test]
    fn a_held_create_lock_is_not_broken_by_age_and_waiters_can_give_up() {
        use std::time::{Duration, SystemTime};
        let d = Dir::new();
        let store = Store::at(d.path().to_path_buf()).unwrap();
        let _g = store.lock_create().unwrap();
        // A long clone keeps the lock for minutes; a live holder is never broken.
        let lock = d.path().join("locks/create.lock");
        let old = SystemTime::now() - Duration::from_secs(600);
        fs::File::options()
            .write(true)
            .open(&lock)
            .unwrap()
            .set_modified(old)
            .unwrap();
        let err = store
            .lock_create_queued(Duration::from_millis(300), &|| false)
            .unwrap_err();
        assert!(err.to_string().contains("timed out"), "{err}");
        let err = store
            .lock_create_queued(Duration::from_secs(60), &|| true)
            .unwrap_err();
        assert!(err.to_string().contains("cancelled"), "{err}");
        // A lock whose holder is dead is broken at once.
        fs::write(&lock, "4294967294").unwrap();
        assert!(
            store
                .lock_create_queued(Duration::from_millis(300), &|| false)
                .is_ok()
        );
    }

    #[test]
    fn instance_lock_is_exclusive_and_breaks_stale_holders() {
        use std::time::Duration;
        let d = Dir::new();
        let store = Store::at(d.path().to_path_buf()).unwrap();
        store.save(&sample("abc123", "one")).unwrap();
        let g = store
            .lock_instance("abc123", Duration::from_secs(1))
            .unwrap();
        let err = store
            .lock_instance("abc123", Duration::from_millis(300))
            .unwrap_err();
        assert!(err.to_string().contains("busy"), "{err}");
        assert_eq!(crate::errors::code_of(&err), crate::errors::CONFLICT);
        drop(g);
        assert!(
            store
                .lock_instance("abc123", Duration::from_secs(1))
                .is_ok(),
            "released on drop"
        );
        // A lock left behind by a dead process is broken.
        let lock = d.path().join("instances/abc123/op.lock");
        fs::write(&lock, "4294967294").unwrap();
        assert!(
            store
                .lock_instance("abc123", Duration::from_secs(1))
                .is_ok()
        );
        assert!(
            store
                .lock_instance("missing", Duration::from_millis(100))
                .is_err()
        );
    }

    #[test]
    fn ids_are_unique() {
        let a: std::collections::HashSet<String> = (0..1000).map(|_| new_id()).collect();
        assert_eq!(a.len(), 1000);
    }
}
