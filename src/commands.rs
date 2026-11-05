use crate::errors;
use crate::images::{self, ImageMeta};
use crate::runtime::{self, BootMode};
use crate::sdk::Sdk;
use crate::store::{Instance, State, Store, new_id, now_secs};
use crate::units::{ImageRef, parse_size_mb};
use anyhow::{Context, Result, bail};
use std::path::Path;
use std::time::Duration;

pub struct RunArgs {
    pub image: String,
    pub name: Option<String>,
    pub detach: bool,
    pub rm: bool,
    pub memory: Option<String>,
    pub cpus: Option<u32>,
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

/// Replace `key = value` (the emulator normalises spacing) in an AVD config.
fn patch_config(path: &std::path::Path, key: &str, value: &str) -> Result<()> {
    let text = std::fs::read_to_string(path)?;
    let mut found = false;
    let mut out: Vec<String> = text
        .lines()
        .map(|l| match l.split_once('=') {
            Some((k, _)) if k.trim() == key => {
                found = true;
                format!("{key}={value}")
            }
            _ => l.to_string(),
        })
        .collect();
    if !found {
        out.push(format!("{key}={value}"));
    }
    std::fs::write(path, out.join("\n") + "\n")?;
    Ok(())
}

pub struct Started {
    pub sdk: Sdk,
    pub store: Store,
    pub inst: Instance,
    pub image: Option<ImageMeta>,
    pub ready_secs: f64,
    pub restored: bool,
}

/// Create, start and wait for an instance. `force_cold` ignores any snapshot.
pub fn start(a: &RunArgs, force_cold: bool) -> Result<Started> {
    let sdk = Sdk::locate().map_err(errors::with(errors::DEPENDENCY))?;
    if !sdk.emulator().is_file() {
        return Err(errors::with(errors::DEPENDENCY)(anyhow::anyhow!(
            "emulator not found at {}\n\nRun `dorker doctor`.",
            sdk.emulator().display()
        )));
    }
    let store = Store::open()?;
    let cfg = errors::with(errors::CONFIG);
    let dep = errors::with(errors::DEPENDENCY);

    // Resolve what to run: a base `android:<api>` image or a built image.
    let image_meta = if a.image.starts_with("android:") {
        None
    } else {
        Some(images::find(&store, &a.image).map_err(cfg)?)
    };
    let (sys, mut memory_mb, mut cpus, disk_mb, headless, device, (width, height)) =
        match &image_meta {
            Some(m) => {
                let r = ImageRef {
                    api: m.api,
                    flavour: Some(m.flavour.clone()),
                };
                let sys = sdk
                    .resolve(&r)
                    .with_context(|| format!("image {} needs {}", m.id, m.system_image))
                    .map_err(dep)?;
                (
                    sys,
                    m.memory_mb,
                    m.cpus,
                    m.disk_mb,
                    m.headless,
                    m.device.clone(),
                    (m.width, m.height),
                )
            }
            None => {
                let sys = sdk
                    .resolve(&ImageRef::parse(&a.image).map_err(cfg)?)
                    .map_err(dep)?;
                (
                    sys,
                    2048,
                    2,
                    4096,
                    true,
                    "pixel_7".to_string(),
                    (1080, 2400),
                )
            }
        };
    let mut overridden = false;
    if let Some(m) = &a.memory {
        let mb = parse_size_mb(m).map_err(cfg)?;
        overridden |= mb != memory_mb;
        memory_mb = mb;
    }
    if let Some(c) = a.cpus {
        overridden |= c != cpus;
        cpus = c;
    }
    if memory_mb < 1024 {
        return Err(cfg(anyhow::anyhow!(
            "--memory must be at least 1GB (got {memory_mb}MB)"
        )));
    }
    if cpus == 0 || cpus > host_cpus() {
        return Err(cfg(anyhow::anyhow!(
            "--cpus must be between 1 and {} (host cores)",
            host_cpus()
        )));
    }
    if let Some(n) = &a.name {
        if !valid_name(n) {
            return Err(cfg(anyhow::anyhow!(
                "invalid --name {n:?}: use letters, digits, '-', '_' or '.'"
            )));
        }
    }

    // Decide the boot mode. A snapshot is only restored when everything the
    // emulator compares (hardware config, system image, emulator build) matches.
    let mut boot = BootMode::Cold;
    let mut snapshot = None;
    if let Some(m) = image_meta.as_ref().filter(|_| !force_cold) {
        if let Some(snap) = &m.snapshot {
            let rev = sdk.system_image_revision(&sys).unwrap_or_default();
            let emu = sdk.emulator_version().unwrap_or_default();
            if overridden {
                eprintln!("note: resource overrides invalidate the snapshot; cold booting");
            } else if rev != m.system_image_revision {
                eprintln!(
                    "note: system image changed ({} -> {rev}); snapshot is incompatible, cold booting",
                    m.system_image_revision
                );
            } else if emu != m.emulator_version {
                eprintln!(
                    "note: emulator changed ({} -> {emu}); snapshot is incompatible, cold booting",
                    m.emulator_version
                );
            } else {
                boot = BootMode::Restore(snap.clone());
                snapshot = Some(snap.clone());
            }
        }
    }

    runtime::install_interrupt_handler();

    // Name/port allocation and process spawn happen under one lock so that
    // concurrent `dorker run` invocations cannot collide.
    let lock = store
        .lock_create()
        .map_err(errors::with(errors::CONFLICT))?;
    let id = new_id();
    let name = a
        .name
        .clone()
        .unwrap_or_else(|| format!("android-{}", &id[..6]));
    if store.load_all()?.iter().any(|i| i.name == name) {
        return Err(errors::with(errors::CONFLICT)(anyhow::anyhow!(
            "instance name {name:?} is already in use\n\nRun `dorker rm {name}` or pick another --name."
        )));
    }
    let console_port =
        runtime::allocate_port(&store, a.adb_port).map_err(errors::with(errors::CONFLICT))?;

    let mut inst = Instance {
        id,
        name,
        image: a.image.clone(),
        api: sys.api,
        flavour: sys.flavour.clone(),
        abi: sys.abi.clone(),
        memory_mb,
        cpus,
        disk_mb,
        headless,
        auto_remove: a.rm,
        console_port,
        pid: None,
        state: State::Created,
        created_at: now_secs(),
        device,
        width,
        height,
        image_id: image_meta.as_ref().map(|m| m.id.clone()),
        snapshot,
    };
    store.save(&inst)?;

    let started = (|| -> Result<()> {
        match &image_meta {
            Some(m) => {
                let dst = store.instance_dir(&inst.id).join("avd");
                runtime::clone_avd_home(&images::avd_home(&store, &m.id), &dst, m.api)?;
                if overridden {
                    let cfg = dst
                        .join(format!("{}.avd", runtime::AVD_NAME))
                        .join("config.ini");
                    patch_config(&cfg, "hw.ramSize", &memory_mb.to_string())?;
                    patch_config(&cfg, "hw.cpu.ncore", &cpus.to_string())?;
                }
            }
            None => runtime::write_avd(&store, &inst, &sys)?,
        }
        inst.set_state(State::Starting)?;
        inst.pid = Some(runtime::spawn_emulator(&sdk, &store, &inst, &boot)?);
        store.save(&inst)
    })();
    drop(lock);
    if let Err(e) = started {
        let _ = store.remove(&inst); // never leave a half-created instance behind
        return Err(errors::with(errors::STARTUP)(e));
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
    let wait = |store: &Store, inst: &mut Instance| {
        runtime::wait_ready(
            &sdk,
            store,
            inst,
            Duration::from_secs(a.timeout),
            runtime::interrupted,
            || eprintln!("  Android is booting..."),
        )
    };
    let mut ready = wait(&store, &mut inst);
    if ready.is_err() && matches!(boot, BootMode::Restore(_)) && !runtime::interrupted() {
        // A corrupt or unloadable snapshot must not make the image unusable.
        eprintln!("warning: snapshot restore failed; falling back to a cold boot");
        let retry = (|| -> Result<()> {
            inst.set_state(State::Starting)?;
            inst.snapshot = None;
            inst.pid = Some(runtime::spawn_emulator(
                &sdk,
                &store,
                &inst,
                &BootMode::Cold,
            )?);
            store.save(&inst)
        })();
        ready = match retry {
            Ok(()) => wait(&store, &mut inst),
            Err(e) => Err(e),
        };
    }
    if let Err(e) = ready {
        if a.rm {
            let _ = store.remove(&inst);
        }
        return Err(errors::with(errors::STARTUP)(e));
    }
    let ready_secs = t0.elapsed().as_secs_f64();
    let restored = runtime::restored_from_snapshot(&store, &inst);
    eprintln!(
        "Ready in {ready_secs:.1}s ({})",
        if restored {
            "snapshot restore"
        } else {
            "cold boot"
        }
    );
    Ok(Started {
        sdk,
        store,
        inst,
        image: image_meta,
        ready_secs,
        restored,
    })
}

pub fn run(a: RunArgs) -> Result<i32> {
    let Started {
        sdk,
        store,
        mut inst,
        image,
        ..
    } = start(&a, false)?;

    if let Some(pkg) = image.as_ref().and_then(|m| m.launch.as_ref()) {
        let launched = runtime::adb(&sdk, &inst.serial())
            .args([
                "shell",
                "monkey",
                "-p",
                pkg,
                "-c",
                "android.intent.category.LAUNCHER",
                "1",
            ])
            .output();
        match launched {
            Ok(o) if o.status.success() => eprintln!("Launched {pkg}"),
            _ => eprintln!("warning: could not launch {pkg}; is it installed in the image?"),
        }
    }

    if a.detach {
        println!("{}", inst.id);
        return Ok(0);
    }

    // Foreground: stay attached until the emulator exits or we are interrupted.
    eprintln!("Attached. Press Ctrl-C to stop {}.", inst.name);
    while !runtime::interrupted() && runtime::is_our_process(&inst) {
        std::thread::sleep(Duration::from_millis(500));
    }
    stop_instance(&sdk, &store, &mut inst)?;
    Ok(0)
}

pub fn images(json: bool) -> Result<i32> {
    let store = Store::open()?;
    let list = images::list(&store)?;
    if json {
        let rows: Vec<_> = list
            .iter()
            .map(|m| {
                let (logical, allocated) = images::dir_usage(&images::image_dir(&store, &m.id));
                serde_json::json!({
                    "id": m.id, "tags": m.tags, "api": m.api, "flavour": m.flavour, "abi": m.abi,
                    "snapshot": m.snapshot, "logical_bytes": logical, "allocated_bytes": allocated,
                    "created_at": m.created_at,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(0);
    }
    println!(
        "{:<22}{:<12}{:<14}{:<9}{:<10}SIZE (disk / logical)",
        "REPOSITORY", "TAG", "IMAGE ID", "ANDROID", "SNAPSHOT"
    );
    for m in &list {
        let (logical, allocated) = images::dir_usage(&images::image_dir(&store, &m.id));
        let size = format!("{} / {}", images::human(allocated), images::human(logical));
        let snap = m.snapshot.as_deref().unwrap_or("-");
        let tags: Vec<(String, String)> = if m.tags.is_empty() {
            vec![("<none>".into(), "<none>".into())]
        } else {
            m.tags
                .iter()
                .map(|t| {
                    t.split_once(':')
                        .map(|(r, t)| (r.to_string(), t.to_string()))
                        .unwrap_or((t.clone(), "latest".into()))
                })
                .collect()
        };
        for (repo, tag) in tags {
            println!(
                "{:<22}{:<12}{:<14}{:<9}{:<10}{}",
                repo, tag, m.id, m.api, snap, size
            );
        }
    }
    Ok(0)
}

pub fn rmi(key: &str) -> Result<i32> {
    let store = Store::open()?;
    let meta = images::find(&store, key)?;
    images::remove(&store, &meta)?;
    println!("{}", meta.id);
    Ok(0)
}

pub fn init() -> Result<i32> {
    let path = std::path::Path::new("Dorkerfile");
    if path.exists() {
        return Err(errors::with(errors::CONFLICT)(anyhow::anyhow!(
            "Dorkerfile already exists; not overwriting it"
        )));
    }
    std::fs::write(
        path,
        "FROM android:35\n\nMEMORY 2GB\nCPUS 2\nDISK 4GB\n\nDEVICE pixel_7\nHEADLESS true\n\n\
         # INSTALL ./app/build/outputs/apk/debug/app-debug.apk\n\n\
         SETTING window_animation_scale 0\nSETTING transition_animation_scale 0\nSETTING animator_duration_scale 0\n\n\
         SNAPSHOT ready\n",
    )?;
    println!(
        "Created Dorkerfile.\n\nNext:\n  dorker doctor\n  dorker build -t myapp:latest .\n  dorker run -d --name test myapp:latest\n\nSee docs/dorkerfile.md for every directive."
    );
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
    Ok(if status.success() { 0 } else { errors::ADB })
}

pub fn screenshot(key: &str, output: &Path) -> Result<i32> {
    let (sdk, inst) = ready_instance(key)?;
    let out = runtime::adb(&sdk, &inst.serial())
        .args(["exec-out", "screencap", "-p"])
        .output()
        .context("running adb")?;
    if !out.status.success() || !out.stdout.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err(errors::with(errors::ADB)(anyhow::anyhow!(
            "screencap did not return a PNG: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
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

pub fn stop_instance(sdk: &Sdk, store: &Store, inst: &mut Instance) -> Result<()> {
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

/// Apply `f` to every key, reporting failures but continuing; exit 1 if any failed.
pub fn each(keys: &[String], f: impl Fn(&str) -> Result<i32>) -> Result<i32> {
    let mut code = 0;
    for k in keys {
        match f(k) {
            Ok(c) => code = code.max(c),
            Err(e) => {
                eprintln!("error: {e:#}");
                code = code.max(1);
            }
        }
    }
    Ok(code)
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
