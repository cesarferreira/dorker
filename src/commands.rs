use crate::runtime;
use crate::sdk::Sdk;
use crate::store::{Instance, State, Store, new_id, now_secs};
use crate::units::{ImageRef, parse_size_mb};
use anyhow::{Context, Result, bail};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

pub struct RunArgs {
    pub image: String,
    pub name: Option<String>,
    pub detach: bool,
    pub rm: bool,
    pub memory: String,
    pub cpus: u32,
    pub adb_port: Option<u16>,
    pub timeout: u64,
}

fn host_cpus() -> u32 {
    std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(1)
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

pub fn run(a: RunArgs) -> Result<i32> {
    let sdk = Sdk::locate()?;
    let image_ref = ImageRef::parse(&a.image)?;
    let image = sdk.resolve(&image_ref)?;

    let memory_mb = parse_size_mb(&a.memory)?;
    if memory_mb < 1024 {
        bail!("--memory must be at least 1GB (got {memory_mb}MB)");
    }
    if a.cpus == 0 || a.cpus > host_cpus() {
        bail!("--cpus must be between 1 and {} (host cores)", host_cpus());
    }
    if let Some(n) = &a.name {
        if !valid_name(n) {
            bail!("invalid --name {n:?}: use letters, digits, '-', '_' or '.'");
        }
    }
    if !sdk.emulator().is_file() {
        bail!(
            "emulator not found at {}\n\nRun `dorker doctor`.",
            sdk.emulator().display()
        );
    }

    let store = Store::open()?;
    let _ = ctrlc::set_handler(|| INTERRUPTED.store(true, Ordering::SeqCst));

    // Name/port allocation and process spawn happen under one lock so that
    // concurrent `dorker run` invocations cannot collide.
    let lock = store.lock_create()?;
    let id = new_id();
    let name = a
        .name
        .clone()
        .unwrap_or_else(|| format!("android-{}", &id[..6]));
    if store.load_all()?.iter().any(|i| i.name == name) {
        bail!(
            "instance name {name:?} is already in use\n\nRun `dorker rm {name}` or pick another --name."
        );
    }
    let console_port = runtime::allocate_port(&store, a.adb_port)?;

    let mut inst = Instance {
        id,
        name,
        image: a.image.clone(),
        api: image.api,
        flavour: image.flavour.clone(),
        abi: image.abi.clone(),
        memory_mb,
        cpus: a.cpus,
        disk_mb: 4096,
        headless: true,
        auto_remove: a.rm,
        console_port,
        pid: None,
        state: State::Created,
        created_at: now_secs(),
    };
    store.save(&inst)?;

    let started = (|| -> Result<()> {
        runtime::write_avd(&store, &inst, &image)?;
        inst.set_state(State::Starting)?;
        inst.pid = Some(runtime::spawn_emulator(&sdk, &store, &inst)?);
        store.save(&inst)
    })();
    drop(lock);
    if let Err(e) = started {
        let _ = store.remove(&inst); // never leave a half-created instance behind
        return Err(e);
    }

    eprintln!(
        "Starting {} ({}, API {} {}, {}MB, {} cpus) on {}",
        inst.name,
        inst.id,
        inst.api,
        inst.flavour,
        inst.memory_mb,
        inst.cpus,
        inst.serial()
    );
    let t0 = std::time::Instant::now();
    let ready = runtime::wait_ready(
        &sdk,
        &store,
        &mut inst,
        Duration::from_secs(a.timeout),
        || INTERRUPTED.load(Ordering::SeqCst),
        || eprintln!("  Android is booting..."),
    );
    if let Err(e) = ready {
        if a.rm {
            let _ = store.remove(&inst);
        }
        return Err(e);
    }
    eprintln!("Ready in {:.1}s", t0.elapsed().as_secs_f64());

    if a.detach {
        println!("{}", inst.id);
        return Ok(0);
    }

    // Foreground: stay attached until the emulator exits or we are interrupted.
    eprintln!("Attached. Press Ctrl-C to stop {}.", inst.name);
    while !INTERRUPTED.load(Ordering::SeqCst) && runtime::is_our_process(&inst) {
        std::thread::sleep(Duration::from_millis(500));
    }
    stop_instance(&sdk, &store, &mut inst)?;
    Ok(0)
}

pub fn ps(all: bool, json: bool) -> Result<i32> {
    let store = Store::open()?;
    let mut list = store.load_all()?;
    for i in &mut list {
        runtime::reconcile(&store, i)?;
    }
    if !all {
        list.retain(|i| i.state.expects_process());
    }
    if json {
        let rows: Vec<_> = list
            .iter()
            .map(|i| {
                serde_json::json!({
                    "id": i.id, "name": i.name, "image": i.image, "status": i.state.as_str(),
                    "serial": i.serial(), "adb_port": i.adb_port(), "pid": i.pid,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(0);
    }
    println!(
        "{:<14}{:<20}{:<22}{:<10}ADB",
        "ID", "NAME", "IMAGE", "STATUS"
    );
    for i in &list {
        let adb = if i.state.expects_process() {
            i.serial()
        } else {
            "-".into()
        };
        println!(
            "{:<14}{:<20}{:<22}{:<10}{}",
            &i.id[..12.min(i.id.len())],
            i.name,
            i.image,
            i.state.as_str(),
            adb
        );
    }
    Ok(0)
}

/// Look up an instance and require it to be READY (verified against the process).
fn ready_instance(key: &str) -> Result<(Sdk, Instance)> {
    let sdk = Sdk::locate()?;
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
    Ok((sdk, inst))
}

fn code_of(status: std::process::ExitStatus) -> i32 {
    status.code().unwrap_or(1)
}

pub fn exec(key: &str, command: &[String]) -> Result<i32> {
    let (sdk, inst) = ready_instance(key)?;
    let status = runtime::adb(&sdk, &inst.serial())
        .args(["shell", &command.join(" ")])
        .status()
        .context("running adb")?;
    Ok(code_of(status))
}

pub fn install(key: &str, apk: &Path) -> Result<i32> {
    if !apk.is_file() {
        bail!("APK not found: {}", apk.display());
    }
    let (sdk, inst) = ready_instance(key)?;
    let status = runtime::adb(&sdk, &inst.serial())
        .args(["install", "-r"])
        .arg(apk)
        .status()
        .context("running adb")?;
    Ok(code_of(status))
}

pub fn screenshot(key: &str, output: &Path) -> Result<i32> {
    let (sdk, inst) = ready_instance(key)?;
    let out = runtime::adb(&sdk, &inst.serial())
        .args(["exec-out", "screencap", "-p"])
        .output()
        .context("running adb")?;
    if !out.status.success() || !out.stdout.starts_with(b"\x89PNG\r\n\x1a\n") {
        bail!(
            "screencap did not return a PNG: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    std::fs::write(output, &out.stdout).with_context(|| format!("writing {}", output.display()))?;
    eprintln!("Saved {} ({} bytes)", output.display(), out.stdout.len());
    Ok(0)
}

pub fn logs(key: &str, follow: bool) -> Result<i32> {
    let (sdk, inst) = ready_instance(key)?;
    let mut cmd = runtime::adb(&sdk, &inst.serial());
    cmd.arg("logcat");
    if !follow {
        cmd.arg("-d");
    }
    Ok(code_of(cmd.status().context("running adb")?))
}

fn stop_instance(sdk: &Sdk, store: &Store, inst: &mut Instance) -> Result<()> {
    runtime::reconcile(store, inst)?;
    if inst.state.expects_process() {
        inst.set_state(State::Stopping)?;
        store.save(inst)?;
        runtime::kill_instance(sdk, inst, Duration::from_secs(15));
        inst.set_state(State::Stopped)?;
        inst.pid = None;
        store.save(inst)?;
    }
    if inst.auto_remove {
        store.remove(inst)?;
    }
    Ok(())
}

pub fn stop(key: &str) -> Result<i32> {
    let sdk = Sdk::locate()?;
    let store = Store::open()?;
    let mut inst = store.find(key)?;
    stop_instance(&sdk, &store, &mut inst)?;
    println!("{}", inst.name);
    Ok(0)
}

pub fn rm(key: &str, force: bool) -> Result<i32> {
    let store = Store::open()?;
    let mut inst = store.find(key)?;
    runtime::reconcile(&store, &mut inst)?;
    if inst.state.expects_process() {
        if !force {
            bail!(
                "instance {} is {}; stop it first or use `dorker rm -f`",
                inst.name,
                inst.state.as_str()
            );
        }
        let sdk = Sdk::locate()?;
        inst.auto_remove = false;
        stop_instance(&sdk, &store, &mut inst)?;
    }
    store.remove(&inst)?;
    println!("{}", inst.name);
    Ok(0)
}
