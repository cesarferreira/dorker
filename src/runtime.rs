//! Official-emulator backend: AVD materialisation, process control, ADB.

use crate::sdk::{Sdk, SystemImage};
use crate::store::{Instance, State, Store};
use anyhow::{Context, Result, bail};
use std::net::TcpListener;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const FIRST_PORT: u16 = 5554;
pub const LAST_PORT: u16 = 5680;

// ---------- AVD ----------

/// Render the AVD `config.ini` for an instance. Pure, so it is unit-tested.
pub fn render_avd_config(inst: &Instance, image: &SystemImage) -> String {
    let (w, h, density) = (1080, 2400, 420);
    let gpu_mode = if inst.headless {
        "swiftshader_indirect"
    } else {
        "auto"
    };
    let lines = [
        format!("AvdId=dorker-{}", inst.id),
        format!("avd.ini.displayname=dorker {}", inst.name),
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
        "hw.device.name=pixel_7".to_string(),
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
        "fastboot.forceColdBoot=yes".to_string(),
    ];
    lines.join("\n") + "\n"
}

/// Create a private AVD home so the emulator never sees or touches `~/.android/avd`.
pub fn write_avd(store: &Store, inst: &Instance, image: &SystemImage) -> Result<()> {
    let home = store.instance_dir(&inst.id).join("avd");
    let avd_dir = home.join(format!("dorker-{}.avd", inst.id));
    std::fs::create_dir_all(&avd_dir)?;
    std::fs::write(avd_dir.join("config.ini"), render_avd_config(inst, image))?;
    std::fs::write(
        home.join(format!("dorker-{}.ini", inst.id)),
        format!(
            "avd.ini.encoding=UTF-8\npath={}\ntarget=android-{}\n",
            avd_dir.display(),
            image.api
        ),
    )?;
    Ok(())
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
            bail!(
                "--adb-port must be an even console port in {FIRST_PORT}..={LAST_PORT} (adb uses port+1)"
            );
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

/// True only if `pid` is alive AND is the emulator we launched for this instance.
/// Guards against PID reuse before signalling anything.
pub fn is_our_process(inst: &Instance) -> bool {
    let Some(pid) = inst.pid else { return false };
    match ps_info(pid) {
        Some((_, cmd)) => cmd.contains(&format!("dorker-{}", inst.id)),
        None => false,
    }
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
    if inst.state.expects_process() && !is_our_process(inst) {
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

pub fn spawn_emulator(sdk: &Sdk, store: &Store, inst: &Instance) -> Result<u32> {
    let dir = store.instance_dir(&inst.id);
    let log = std::fs::File::create(dir.join("emulator.log"))?;
    let log_err = log.try_clone()?;

    let mut cmd = Command::new(sdk.emulator());
    cmd.arg("-avd")
        .arg(format!("dorker-{}", inst.id))
        .args(["-port", &inst.console_port.to_string()])
        .args(["-no-snapshot", "-no-audio", "-no-boot-anim", "-no-metrics"])
        .args(["-netdelay", "none", "-netspeed", "full"]);
    if inst.headless {
        cmd.args(["-no-window", "-gpu", "swiftshader_indirect"]);
    }
    cmd.env("ANDROID_AVD_HOME", dir.join("avd"))
        .env("ANDROID_SDK_ROOT", &sdk.root)
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(log_err)
        // Own process group so we can signal launcher + qemu together and so
        // Ctrl-C in the CLI does not hit the emulator directly.
        .process_group(0);
    let child = cmd
        .spawn()
        .with_context(|| format!("starting {}", sdk.emulator().display()))?;
    Ok(child.id())
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
        if !is_our_process(inst) {
            inst.set_state(State::Failed)?;
            inst.pid = None;
            store.save(inst)?;
            bail!(
                "emulator exited during startup\n\n{}",
                log_tail(&store.instance_dir(&inst.id).join("emulator.log"), 15)
            );
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
            inst.set_state(State::Booting)?;
            store.save(inst)?;
            on_booting();
        }
        if seen_device && is_ready(sdk, &serial) {
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

/// Stop the emulator: graceful `adb emu kill`, then SIGTERM, then SIGKILL.
/// Only signals a process verified to be this instance's emulator.
pub fn kill_instance(sdk: &Sdk, inst: &Instance, grace: Duration) {
    let Some(pid) = inst.pid else { return };
    if !is_our_process(inst) {
        return;
    }
    let _ = adb(sdk, &inst.serial()).args(["emu", "kill"]).output();
    if wait_gone(inst, grace) {
        return;
    }
    signal_group(pid, "-TERM");
    if wait_gone(inst, Duration::from_secs(5)) {
        return;
    }
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
        }
    }

    fn image() -> SystemImage {
        SystemImage {
            api: 35,
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
        assert!(home.join("dorker-abc123def456.ini").is_file());
        assert!(home.join("dorker-abc123def456.avd/config.ini").is_file());
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
    fn pid_identity_is_checked() {
        let mut a = inst();
        // Our own pid is alive but is not an emulator for this instance.
        a.pid = Some(std::process::id());
        assert!(!is_our_process(&a));
    }
}
