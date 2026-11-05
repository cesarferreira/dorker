//! Official-emulator backend: AVD materialisation, process control, ADB.

use crate::sdk::{Sdk, SystemImage};
use crate::store::{Instance, State, Store};
use anyhow::{Context, Result, bail};
use std::net::TcpListener;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Every instance uses the same AVD name: the emulator refuses to restore a
/// snapshot into an AVD with a different identity ("different AVD configuration").
/// Isolation comes from the private `ANDROID_AVD_HOME` directory instead.
pub const AVD_NAME: &str = "dorker";

/// Set by Ctrl-C; long operations poll it and clean up.
pub static INTERRUPTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn install_interrupt_handler() {
    let _ = ctrlc::set_handler(|| INTERRUPTED.store(true, std::sync::atomic::Ordering::SeqCst));
}

pub fn interrupted() -> bool {
    INTERRUPTED.load(std::sync::atomic::Ordering::SeqCst)
}

pub const FIRST_PORT: u16 = 5554;
pub const LAST_PORT: u16 = 5680;

// ---------- AVD ----------

/// Render the AVD `config.ini` for an instance. Pure, so it is unit-tested.
pub fn render_avd_config(inst: &Instance, image: &SystemImage) -> String {
    let (w, h) = (inst.width, inst.height);
    let density = 420;
    let gpu_mode = if inst.headless {
        "swiftshader_indirect"
    } else {
        "auto"
    };
    let lines = [
        format!("AvdId={AVD_NAME}"),
        format!("avd.ini.displayname={AVD_NAME}"),
        "avd.ini.encoding=UTF-8".to_string(),
        format!("abi.type={}", image.abi),
        format!(
            "hw.cpu.arch={}",
            if image.abi == "arm64-v8a" {
                "arm64"
            } else {
                "x86_64"
            }
        ),
        format!("hw.cpu.ncore={}", inst.cpus),
        format!("hw.ramSize={}", inst.memory_mb),
        format!("disk.dataPartition.size={}M", inst.disk_mb),
        format!("hw.device.name={}", inst.device),
        "hw.device.manufacturer=Google".to_string(),
        format!("hw.lcd.width={w}"),
        format!("hw.lcd.height={h}"),
        format!("hw.lcd.density={density}"),
        "hw.gpu.enabled=yes".to_string(),
        format!("hw.gpu.mode={gpu_mode}"),
        "hw.keyboard=yes".to_string(),
        "hw.sdCard=no".to_string(),
        "showDeviceFrame=no".to_string(),
        format!("image.sysdir.1={}", image.sysdir),
        format!("tag.id={}", image.flavour),
        format!("tag.display={}", image.flavour),
        format!("target=android-{}", image.api),
        "fastboot.forceColdBoot=no".to_string(),
    ];
    lines.join("\n") + "\n"
}

/// Create a private AVD home so the emulator never sees or touches `~/.android/avd`.
pub fn write_avd(store: &Store, inst: &Instance, image: &SystemImage) -> Result<()> {
    let home = store.instance_dir(&inst.id).join("avd");
    let avd_dir = home.join(format!("{AVD_NAME}.avd"));
    std::fs::create_dir_all(&avd_dir)?;
    std::fs::write(avd_dir.join("config.ini"), render_avd_config(inst, image))?;
    write_avd_ini(&home, image.api)
}

fn write_avd_ini(home: &Path, api: u32) -> Result<()> {
    std::fs::write(
        home.join(format!("{AVD_NAME}.ini")),
        format!(
            "avd.ini.encoding=UTF-8\npath={}\ntarget=android-{}\n",
            home.join(format!("{AVD_NAME}.avd")).display(),
            api
        ),
    )?;
    Ok(())
}

/// Copy an image's AVD home to a new location using filesystem clones where
/// available (APFS `cp -c`, btrfs/xfs reflinks), falling back to a plain copy.
/// Rewrites the `.ini` path so the copy is self-contained.
pub fn clone_avd_home(src: &Path, dst: &Path, api: u32) -> Result<()> {
    if dst.exists() {
        bail!("{} already exists", dst.display());
    }
    let attempts: [&[&str]; 2] = if cfg!(target_os = "macos") {
        [&["-c", "-R"], &["-R"]]
    } else {
        [&["-R", "--reflink=auto"], &["-R"]]
    };
    let mut copied = false;
    for flags in attempts {
        let ok = Command::new("cp")
            .args(flags)
            .arg(src)
            .arg(dst)
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            copied = true;
            break;
        }
        let _ = std::fs::remove_dir_all(dst);
    }
    if !copied {
        bail!("copying {} to {} failed", src.display(), dst.display());
    }
    write_avd_ini(dst, api)
}

// ---------- ports ----------

fn port_free(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_ok()
}

/// Pick an even console port whose console and ADB ports are both free and not
/// claimed by another dorker instance. Caller must hold the create lock.
pub fn allocate_port(store: &Store, requested: Option<u16>) -> Result<u16> {
    let taken: Vec<u16> = store
        .load_all()?
        .iter()
        .filter(|i| i.state.expects_process())
        .map(|i| i.console_port)
        .collect();
    let usable = |p: u16| !taken.contains(&p) && port_free(p) && port_free(p + 1);
    if let Some(p) = requested {
        if p % 2 != 0 || !(FIRST_PORT..=LAST_PORT).contains(&p) {
            return Err(crate::errors::with(crate::errors::CONFIG)(anyhow::anyhow!(
                "--adb-port must be an even console port in {FIRST_PORT}..={LAST_PORT} (adb uses port+1)"
            )));
        }
        if !usable(p) {
            bail!("port {p} (or {}) is already in use", p + 1);
        }
        return Ok(p);
    }
    (FIRST_PORT..=LAST_PORT)
        .step_by(2)
        .find(|p| usable(*p))
        .context("no free emulator port pair available")
}

// ---------- processes ----------

/// `ps` row for a pid: (process group id, full command line).
fn ps_info(pid: u32) -> Option<(u32, String)> {
    let out = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "pgid=,command="])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.trim();
    let (pgid, cmd) = line.split_once(char::is_whitespace)?;
    Some((pgid.trim().parse().ok()?, cmd.trim().to_string()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcState {
    /// Alive and running this instance's emulator.
    Ours,
    /// Alive but not (yet) the emulator: a reused pid, or a freshly forked child
    /// that has not finished `exec`.
    Foreign,
    /// Gone (or a zombie).
    Dead,
}

/// Full command line of a process. On Linux `/proc/<pid>/cmdline` is exact; `ps`
/// can truncate and has to be spawned, so it is only the fallback (macOS).
fn cmdline_of(pid: u32) -> Option<String> {
    if let Ok(bytes) = std::fs::read(format!("/proc/{pid}/cmdline")) {
        if !bytes.is_empty() {
            return Some(
                String::from_utf8_lossy(&bytes)
                    .replace('\0', " ")
                    .trim()
                    .to_string(),
            );
        }
    }
    ps_info(pid).map(|(_, cmd)| cmd)
}

pub fn process_state(inst: &Instance) -> ProcState {
    let Some(pid) = inst.pid else {
        return ProcState::Dead;
    };
    match cmdline_of(pid) {
        None => ProcState::Dead,
        Some(cmd) if cmd.contains("<defunct>") => ProcState::Dead,
        Some(cmd) if cmd.contains(&format!("-avd {AVD_NAME} -port {}", inst.console_port)) => {
            ProcState::Ours
        }
        Some(_) => ProcState::Foreign,
    }
}

/// True only if `pid` is alive AND is the emulator we launched for this instance.
/// Guards against PID reuse before signalling anything.
pub fn is_our_process(inst: &Instance) -> bool {
    process_state(inst) == ProcState::Ours
}

fn signal_group(pid: u32, sig: &str) {
    // Negative pid = the whole process group (launcher + qemu child).
    let _ = Command::new("kill")
        .args([sig, "--", &format!("-{pid}")])
        .status();
}

/// Reconcile persisted state with reality: a process-expecting state with a dead
/// process becomes CRASHED (or STOPPED if we were stopping).
pub fn reconcile(store: &Store, inst: &mut Instance) -> Result<()> {
    // A CLI killed between creating the record and spawning the emulator leaves a
    // CREATED record with no process; it can never become usable, so fail it.
    if inst.state == State::Created && crate::store::now_secs().saturating_sub(inst.created_at) > 60
    {
        inst.set_state(State::Failed)?;
        store.save(inst)?;
        return Ok(());
    }
    let pstate = process_state(inst);
    // A STARTING instance whose pid is still a pre-exec copy of the CLI is not dead.
    let starting_pre_exec = inst.state == State::Starting && pstate == ProcState::Foreign;
    if inst.state.expects_process() && pstate != ProcState::Ours && !starting_pre_exec {
        let next = if inst.state == State::Stopping {
            State::Stopped
        } else {
            State::Crashed
        };
        inst.set_state(next)?;
        inst.pid = None;
        store.save(inst)?;
    }
    Ok(())
}

/// Emulator processes that look like dorker's (`-avd dorker -port N`) but belong to
/// no instance record, e.g. after the CLI was killed mid-start. Returns (pid, port).
pub fn orphans(store: &Store) -> Vec<(u32, u16)> {
    let known: Vec<u32> = store
        .load_all()
        .unwrap_or_default()
        .iter()
        .filter_map(|i| i.pid)
        .collect();
    let Ok(out) = Command::new("ps")
        .args(["-ax", "-o", "pid=,command="])
        .output()
    else {
        return vec![];
    };
    parse_orphans(&String::from_utf8_lossy(&out.stdout), &known)
}

pub fn parse_orphans(ps_output: &str, known_pids: &[u32]) -> Vec<(u32, u16)> {
    ps_output
        .lines()
        .filter_map(|l| {
            let l = l.trim();
            let (pid, cmd) = l.split_once(char::is_whitespace)?;
            let pid: u32 = pid.parse().ok()?;
            let port = cmd
                .split("-avd dorker -port ")
                .nth(1)?
                .split_whitespace()
                .next()?
                .parse()
                .ok()?;
            (!known_pids.contains(&pid)).then_some((pid, port))
        })
        .collect()
}

// ---------- adb ----------

pub fn adb(sdk: &Sdk, serial: &str) -> Command {
    let mut c = Command::new(sdk.adb());
    c.args(["-s", serial]);
    c
}

fn adb_getprop(sdk: &Sdk, serial: &str, prop: &str) -> Option<String> {
    let out = adb(sdk, serial)
        .args(["shell", "getprop", prop])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Android is usable when boot completed AND the package manager answers.
pub fn is_ready(sdk: &Sdk, serial: &str) -> bool {
    adb_getprop(sdk, serial, "sys.boot_completed").as_deref() == Some("1")
        && adb(sdk, serial)
            .args(["shell", "pm", "path", "android"])
            .output()
            .map(|o| o.status.success() && !o.stdout.is_empty())
            .unwrap_or(false)
}

// ---------- start / stop ----------

/// How the emulator should come up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootMode {
    /// Full boot; never load or save snapshots.
    Cold,
    /// Full boot; snapshots can be saved explicitly (used by `build`).
    ColdSaveable,
    /// Restore the named snapshot (falls back to a cold boot if it cannot load);
    /// never saves on exit so the instance copy stays reproducible.
    Restore(String),
}

pub fn spawn_emulator(sdk: &Sdk, store: &Store, inst: &Instance, mode: &BootMode) -> Result<u32> {
    let dir = store.instance_dir(&inst.id);
    let log = std::fs::File::create(dir.join("emulator.log"))?;
    let log_err = log.try_clone()?;

    let mut cmd = Command::new(sdk.emulator());
    cmd.args(["-avd", AVD_NAME])
        .args(["-port", &inst.console_port.to_string()])
        .args(["-no-audio", "-no-boot-anim", "-no-metrics"])
        .args(["-netdelay", "none", "-netspeed", "full"]);
    match mode {
        BootMode::Cold => {
            cmd.arg("-no-snapshot");
        }
        BootMode::ColdSaveable => {
            cmd.args(["-no-snapshot-load", "-no-snapshot-save"]);
        }
        BootMode::Restore(name) => {
            cmd.args(["-snapshot", name, "-no-snapshot-save"]);
        }
    }
    cmd.args(["-gpu", &inst.gpu]);
    if inst.headless {
        cmd.arg("-no-window");
    }
    cmd.env("ANDROID_AVD_HOME", dir.join("avd"))
        .env("ANDROID_SDK_ROOT", &sdk.root)
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(log_err)
        // Own process group so we can signal launcher + qemu together and so
        // Ctrl-C in the CLI does not hit the emulator directly.
        .process_group(0);
    if crate::log::enabled() {
        let args: Vec<std::ffi::OsString> = cmd.get_args().map(|a| a.to_os_string()).collect();
        crate::log::vlog!("boot mode: {mode:?}");
        crate::log::vlog!(
            "env ANDROID_AVD_HOME={} ANDROID_SDK_ROOT={}",
            dir.join("avd").display(),
            sdk.root.display()
        );
        crate::log::vlog!(
            "spawn: {}",
            crate::log::render_command(&sdk.emulator(), &args)
        );
        crate::log::vlog!("emulator output: {}", dir.join("emulator.log").display());
    }
    let child = cmd
        .spawn()
        .with_context(|| format!("starting {}", sdk.emulator().display()))?;
    crate::log::vlog!("spawned pid {}", child.id());
    Ok(child.id())
}

/// Whether the emulator log shows a snapshot was restored.
pub fn restored_from_snapshot(store: &Store, inst: &Instance) -> bool {
    std::fs::read_to_string(store.instance_dir(&inst.id).join("emulator.log"))
        .map(|t| t.contains("Successfully loaded snapshot"))
        .unwrap_or(false)
}

/// Wait for verified readiness. Fails early if the emulator process dies.
pub fn wait_ready(
    sdk: &Sdk,
    store: &Store,
    inst: &mut Instance,
    timeout: Duration,
    cancelled: impl Fn() -> bool,
    on_booting: impl Fn(),
) -> Result<()> {
    let start = Instant::now();
    let mut seen_device = false;
    loop {
        match process_state(inst) {
            ProcState::Ours => {}
            // Right after spawn the child can still be a copy of this process until
            // `exec` finishes (slow on network-backed SDK mounts); give it time.
            ProcState::Foreign if start.elapsed() < Duration::from_secs(15) => {
                crate::log::vlog!(
                    "pid {:?} is not the emulator yet (pre-exec window); waiting",
                    inst.pid
                );
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            state => {
                let seen = inst
                    .pid
                    .and_then(cmdline_of)
                    .unwrap_or_else(|| "no such process".into());
                inst.set_state(State::Failed)?;
                inst.pid = None;
                store.save(inst)?;
                let what = if state == ProcState::Dead {
                    "exited during startup"
                } else {
                    "was replaced by an unrelated process"
                };
                bail!(
                    "emulator {what} (last seen: {seen})\n\n{}",
                    log_tail(&store.instance_dir(&inst.id).join("emulator.log"), 15)
                );
            }
        }
        if cancelled() {
            kill_instance(sdk, inst, Duration::from_secs(5));
            inst.set_state(State::Failed)?;
            inst.pid = None;
            store.save(inst)?;
            bail!("interrupted while waiting for Android to boot");
        }
        let serial = inst.serial();
        if !seen_device && adb_getprop(sdk, &serial, "ro.build.version.sdk").is_some() {
            seen_device = true;
            crate::log::vlog!(
                "adb sees {serial} after {:.1}s; waiting for boot_completed and the package manager",
                start.elapsed().as_secs_f64()
            );
            inst.set_state(State::Booting)?;
            store.save(inst)?;
            on_booting();
        }
        if seen_device && is_ready(sdk, &serial) {
            crate::log::vlog!("{serial} ready after {:.1}s", start.elapsed().as_secs_f64());
            inst.set_state(State::Ready)?;
            store.save(inst)?;
            return Ok(());
        }
        if start.elapsed() > timeout {
            kill_instance(sdk, inst, Duration::from_secs(5));
            inst.set_state(State::Failed)?;
            inst.pid = None;
            store.save(inst)?;
            bail!(
                "timed out after {}s waiting for Android to boot\n\n{}",
                timeout.as_secs(),
                log_tail(&store.instance_dir(&inst.id).join("emulator.log"), 15)
            );
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// Launch a package's launcher activity and wait until its first frame is drawn
/// (`am start -W`), so a screenshot taken right afterwards shows the app and not
/// the system splash screen.
pub fn launch_app(sdk: &Sdk, serial: &str, pkg: &str) -> Result<()> {
    let resolved = adb(sdk, serial)
        .args([
            "shell",
            "cmd",
            "package",
            "resolve-activity",
            "--brief",
            "-a",
            "android.intent.action.MAIN",
            "-c",
            "android.intent.category.LAUNCHER",
            pkg,
        ])
        .output()?;
    let component = String::from_utf8_lossy(&resolved.stdout)
        .lines()
        .map(str::trim)
        .rfind(|l| l.contains('/') && l.starts_with(pkg))
        .map(str::to_string);
    let Some(component) = component else {
        bail!("package {pkg} has no launcher activity (is it installed?)");
    };
    let out = adb(sdk, serial)
        .args(["shell", "am", "start", "-W", "-n", &component])
        .output()?;
    let text = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() || text.contains("Error") || !text.contains("Status: ok") {
        bail!("could not launch {component}: {}", text.trim());
    }
    // `am start -W` can return while the system splash screen still covers the
    // app. The splash is its own window; wait (briefly) until it is gone.
    // `pkg` comes from a validated `CMD launch` package name, so it is shell-safe.
    let probe = format!("dumpsys window windows | grep -c 'Splash Screen {pkg}'");
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let o = adb(sdk, serial).args(["shell", &probe]).output()?;
        if String::from_utf8_lossy(&o.stdout).trim() == "0" {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Ok(())
}

/// Reboot the guest and wait until it is usable again. Rebooting forces Android to
/// flush delayed writes (package database, settings) to the virtual disk, so the
/// state survives a cold boot and not only a snapshot restore.
pub fn reboot_and_wait(sdk: &Sdk, serial: &str, timeout: Duration) -> Result<()> {
    adb(sdk, serial).args(["shell", "sync"]).output()?;
    let _ = adb(sdk, serial).arg("reboot").output();
    let start = Instant::now();
    // Wait for the device to actually go away, otherwise we would see the old boot.
    while adb_getprop(sdk, serial, "sys.boot_completed").is_some() {
        if start.elapsed() > Duration::from_secs(30) {
            bail!("guest did not reboot within 30s");
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    while !is_ready(sdk, serial) {
        if start.elapsed() > timeout {
            bail!(
                "guest did not come back after reboot within {}s",
                timeout.as_secs()
            );
        }
        if interrupted() {
            bail!("interrupted while waiting for the guest to reboot");
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Ok(())
}

/// Stop the emulator: graceful `adb emu kill`, then SIGTERM, then SIGKILL.
/// Only signals a process verified to be this instance's emulator.
pub fn kill_instance(sdk: &Sdk, inst: &Instance, grace: Duration) {
    let Some(pid) = inst.pid else { return };
    if !is_our_process(inst) {
        return;
    }
    // Flush the guest's file system so an instance restarted later keeps its data.
    if let Ok(mut c) = adb(sdk, &inst.serial())
        .args(["shell", "sync"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(5) {
            if matches!(c.try_wait(), Ok(Some(_))) {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = c.kill();
        let _ = c.wait();
    }
    crate::log::vlog!(
        "stopping {} (pid {pid}): adb emu kill, then SIGTERM/SIGKILL if it lingers",
        inst.name
    );
    let _ = adb(sdk, &inst.serial()).args(["emu", "kill"]).output();
    if wait_gone(inst, grace) {
        return;
    }
    crate::log::vlog!(
        "pid {pid} still alive after {}s; sending SIGTERM to its process group",
        grace.as_secs()
    );
    signal_group(pid, "-TERM");
    if wait_gone(inst, Duration::from_secs(5)) {
        return;
    }
    crate::log::vlog!("pid {pid} ignored SIGTERM; sending SIGKILL");
    signal_group(pid, "-KILL");
    wait_gone(inst, Duration::from_secs(5));
}

fn wait_gone(inst: &Instance, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if !is_our_process(inst) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    !is_our_process(inst)
}

pub fn log_tail(path: &Path, lines: usize) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let v: Vec<&str> = text.lines().collect();
    let start = v.len().saturating_sub(lines);
    format!(
        "--- {} (last {} lines) ---\n{}",
        path.display(),
        v.len() - start,
        v[start..].join("\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Dir;

    fn inst() -> Instance {
        Instance {
            id: "abc123def456".into(),
            name: "t".into(),
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
            created_at: 0,
            device: "pixel_7".into(),
            width: 1080,
            height: 2400,
            gpu: "swiftshader_indirect".into(),
            image_id: None,
            snapshot: None,
        }
    }

    fn image() -> SystemImage {
        SystemImage {
            api: 35,
            minor: None,
            flavour: "google_apis".into(),
            abi: "arm64-v8a".into(),
            sysdir: "system-images/android-35/google_apis/arm64-v8a/".into(),
            package: "system-images;android-35;google_apis;arm64-v8a".into(),
        }
    }

    #[test]
    fn avd_config_reflects_resources() {
        let cfg = render_avd_config(&inst(), &image());
        for want in [
            "hw.ramSize=2048",
            "hw.cpu.ncore=2",
            "disk.dataPartition.size=4096M",
            "image.sysdir.1=system-images/android-35/google_apis/arm64-v8a/",
            "hw.cpu.arch=arm64",
            "target=android-35",
        ] {
            assert!(cfg.contains(want), "missing {want}");
        }
    }

    #[test]
    fn avd_is_private_to_instance_dir() {
        let d = Dir::new();
        let store = Store::at(d.path().to_path_buf()).unwrap();
        write_avd(&store, &inst(), &image()).unwrap();
        let home = d.path().join("instances/abc123def456/avd");
        assert!(home.join("dorker.ini").is_file());
        assert!(home.join("dorker.avd/config.ini").is_file());
        let ini = std::fs::read_to_string(home.join("dorker.ini")).unwrap();
        assert!(ini.contains(&format!("path={}", home.join("dorker.avd").display())));
    }

    #[test]
    fn clone_copies_and_repoints_ini() {
        let d = Dir::new();
        let src = d.path().join("src");
        std::fs::create_dir_all(src.join("dorker.avd")).unwrap();
        std::fs::write(src.join("dorker.avd/userdata.img"), b"data").unwrap();
        write_avd_ini(&src, 35).unwrap();
        let dst = d.path().join("dst");
        clone_avd_home(&src, &dst, 35).unwrap();
        assert_eq!(
            std::fs::read(dst.join("dorker.avd/userdata.img")).unwrap(),
            b"data"
        );
        let ini = std::fs::read_to_string(dst.join("dorker.ini")).unwrap();
        assert!(ini.contains(&dst.display().to_string()));
        // Mutating the clone never touches the source.
        std::fs::write(dst.join("dorker.avd/userdata.img"), b"changed").unwrap();
        assert_eq!(
            std::fs::read(src.join("dorker.avd/userdata.img")).unwrap(),
            b"data"
        );
        assert!(clone_avd_home(&src, &dst, 35).is_err());
    }

    #[test]
    fn port_allocation_skips_busy_and_taken() {
        let d = Dir::new();
        let store = Store::at(d.path().to_path_buf()).unwrap();
        let mut a = inst();
        a.state = State::Ready;
        a.console_port = FIRST_PORT;
        store.save(&a).unwrap();
        let p = allocate_port(&store, None).unwrap();
        assert_ne!(p, FIRST_PORT);
        assert_eq!(p % 2, 0);
        assert!(allocate_port(&store, Some(FIRST_PORT)).is_err());
        assert!(allocate_port(&store, Some(5555)).is_err());
        assert!(allocate_port(&store, Some(9000)).is_err());
    }

    #[test]
    fn reconcile_marks_dead_process_crashed() {
        let d = Dir::new();
        let store = Store::at(d.path().to_path_buf()).unwrap();
        let mut a = inst();
        a.state = State::Ready;
        a.pid = Some(u32::MAX - 1); // no such process
        store.save(&a).unwrap();
        reconcile(&store, &mut a).unwrap();
        assert_eq!(a.state, State::Crashed);
        assert_eq!(store.find("t").unwrap().state, State::Crashed);
        assert!(a.pid.is_none());
    }

    #[test]
    fn finds_orphans_only() {
        let ps = "  101 /sdk/emulator/qemu/darwin-aarch64/qemu-system-aarch64-headless -avd dorker -port 5554 -no-audio\n\
  202 /sdk/emulator/emulator -avd dorker -port 5556 -no-window\n\
  303 /sdk/emulator/emulator -avd Pixel_9 -port 5558\n\
  404 vim notes\n";
        assert_eq!(parse_orphans(ps, &[101]), vec![(202, 5556)]);
        assert_eq!(parse_orphans(ps, &[]).len(), 2);
    }

    #[test]
    fn stale_created_record_becomes_failed() {
        let d = Dir::new();
        let store = Store::at(d.path().to_path_buf()).unwrap();
        let mut a = inst();
        a.created_at = 1; // long ago
        store.save(&a).unwrap();
        reconcile(&store, &mut a).unwrap();
        assert_eq!(a.state, State::Failed);
        assert_eq!(store.find("t").unwrap().state, State::Failed);
    }

    #[test]
    fn process_state_distinguishes_ours_foreign_and_dead() {
        let mut a = inst();
        a.console_port = 5998;
        // Our own pid is alive but is not the emulator.
        a.pid = Some(std::process::id());
        assert_eq!(process_state(&a), ProcState::Foreign);
        // A process whose command line carries the instance's emulator arguments.
        let mut child = Command::new("sh")
            .args(["-c", "sleep 30; true # -avd dorker -port 5998"])
            .spawn()
            .unwrap();
        a.pid = Some(child.id());
        // Linux reads /proc, macOS falls back to ps; either way allow exec to finish.
        let mut state = ProcState::Foreign;
        for _ in 0..50 {
            state = process_state(&a);
            if state == ProcState::Ours {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(state, ProcState::Ours);
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(process_state(&a), ProcState::Dead);
        a.pid = None;
        assert_eq!(process_state(&a), ProcState::Dead);
    }

    #[test]
    fn pid_identity_is_checked() {
        let mut a = inst();
        // Our own pid is alive but is not an emulator for this instance.
        a.pid = Some(std::process::id());
        assert!(!is_our_process(&a));
    }
}
