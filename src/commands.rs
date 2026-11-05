use crate::dorkerfile::{GPU_MODES, default_gpu};
use crate::errors;
use crate::images::{self, ImageMeta};
use crate::runtime::{self, BootMode};
use crate::sdk::Sdk;
use crate::store::{Instance, State, Store, new_id, now_secs};
use crate::units::{ImageRef, parse_size_mb};
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
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
    /// GPU mode for base `android:<api>` runs; built images use their own.
    pub gpu: Option<String>,
    /// Print the started instance as JSON on stdout (requires `detach`).
    pub json: bool,
}

/// An instance as machine-readable JSON, for `run --json`, `start --json` and the MCP server.
pub fn instance_json(
    inst: &Instance,
    restored: Option<bool>,
    ready_secs: Option<f64>,
    launched: Option<&str>,
) -> serde_json::Value {
    serde_json::json!({
        "id": inst.id,
        "name": inst.name,
        "image": inst.image,
        "status": inst.state.as_str(),
        "serial": inst.serial(),
        "adb_port": inst.adb_port(),
        "pid": inst.pid,
        "memory_mb": inst.memory_mb,
        "cpus": inst.cpus,
        "gpu": inst.gpu,
        "boot": restored.map(|r| if r { "snapshot" } else { "cold" }),
        "ready_secs": ready_secs.map(|s| (s * 100.0).round() / 100.0),
        "launched": launched,
    })
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

/// How long a command waits for another command operating on the same instance.
const OP_TIMEOUT: Duration = Duration::from_secs(60);

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
    // Cheap, SDK-independent check first: an emulator needs room for userdata and
    // snapshots, and failing here beats the emulator dying halfway through boot.
    let store = Store::open()?;
    runtime::ensure_free_space(&store.root(), 0, "run an instance")?;
    let sdk = Sdk::locate().map_err(errors::with(errors::DEPENDENCY))?;
    if !sdk.emulator().is_file() {
        return Err(errors::with(errors::DEPENDENCY)(anyhow::anyhow!(
            "emulator not found at {}\n\nRun `dorker doctor`.",
            sdk.emulator().display()
        )));
    }
    let cfg = errors::with(errors::CONFIG);
    let dep = errors::with(errors::DEPENDENCY);

    // Resolve what to run: a base `android:<api>` image or a built image.
    let image_meta = if a.image.starts_with("android:") {
        None
    } else {
        Some(images::find(&store, &a.image).map_err(cfg)?)
    };
    let (sys, mut memory_mb, mut cpus, disk_mb, headless, gpu, device, (width, height)) =
        match &image_meta {
            Some(m) => {
                // Resolve by the exact package the image was built from, not by API
                // level, so `android-36` and `android-36.1` can never be confused.
                let sys = sdk
                    .find_package(&m.system_image)
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "image {} needs {} which is not installed\n\nRun:\n  android sdk install \"{}\"",
                            m.id,
                            m.system_image,
                            m.system_image
                        )
                    })
                    .map_err(dep)?;
                (
                    sys,
                    m.memory_mb,
                    m.cpus,
                    m.disk_mb,
                    m.headless,
                    m.gpu.clone(),
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
                    a.gpu
                        .clone()
                        .unwrap_or_else(|| default_gpu(true).to_string()),
                    "pixel_7".to_string(),
                    (1080, 2400),
                )
            }
        };
    // Without filesystem clones (anything but APFS here) an instance is a full copy.
    if let Some(m) = &image_meta {
        if !cfg!(target_os = "macos") {
            let (_, allocated) = images::dir_usage(&images::image_dir(&store, &m.id));
            runtime::ensure_free_space(
                &store.root(),
                allocated,
                "copy the image for a new instance",
            )?;
        }
    }
    if let Some(g) = &a.gpu {
        if image_meta.is_some() {
            return Err(cfg(anyhow::anyhow!(
                "--gpu only applies to base android:<api> runs; a built image's GPU mode is fixed by its Dorkerfile (`GPU {g}`), because it is part of the snapshot"
            )));
        }
        if !GPU_MODES.contains(&g.as_str()) {
            return Err(cfg(anyhow::anyhow!(
                "unsupported --gpu {g:?}: use one of {}",
                GPU_MODES.join(", ")
            )));
        }
    }
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
                crate::log::vlog!(
                    "snapshot {snap:?} is compatible (system image {rev}, emulator {emu}); restoring it"
                );
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
    crate::log::vlog!(
        "allocated console port {console_port} (adb {}), instance {id} named {name:?}",
        console_port + 1
    );

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
        gpu,
        image_id: image_meta.as_ref().map(|m| m.id.clone()),
        snapshot,
    };
    store.save(&inst)?;

    let started = (|| -> Result<()> {
        match &image_meta {
            Some(m) => {
                let dst = store.instance_dir(&inst.id).join("avd");
                crate::log::vlog!("cloning image {} to {}", m.id, dst.display());
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
    if a.json && !a.detach {
        return Err(errors::with(errors::CONFIG)(anyhow::anyhow!(
            "--json needs --detach: a foreground run never returns a result to print"
        )));
    }
    let Started {
        sdk,
        store,
        mut inst,
        image,
        ready_secs,
        restored,
    } = start(&a, false)?;

    let mut launched = None;
    if let Some(pkg) = image.as_ref().and_then(|m| m.launch.as_ref()) {
        match runtime::launch_app(&sdk, &inst.serial(), pkg) {
            Ok(()) => {
                eprintln!("Launched {pkg}");
                launched = Some(pkg.clone());
            }
            Err(e) => eprintln!("warning: {e:#}"),
        }
    }

    if a.detach {
        if a.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&instance_json(
                    &inst,
                    Some(restored),
                    Some(ready_secs),
                    launched.as_deref()
                ))?
            );
        } else {
            println!("{}", inst.id);
        }
        return Ok(0);
    }

    // Foreground: stay attached until the emulator exits or we are interrupted.
    eprintln!("Attached. Press Ctrl-C to stop {}.", inst.name);
    while !runtime::interrupted() && runtime::is_our_process(&inst) {
        std::thread::sleep(Duration::from_millis(500));
    }
    let _op = store.lock_instance(&inst.id, OP_TIMEOUT)?;
    inst = store.find(&inst.id)?;
    stop_instance(&sdk, &store, &mut inst)?;
    Ok(0)
}

/// Restart a stopped, failed or crashed instance. It cold boots from the
/// instance's own userdata, so files it wrote earlier are still there; the image's
/// snapshot is deliberately not restored because it would roll that data back.
pub fn start_existing(key: &str, timeout: u64, json: bool) -> Result<i32> {
    let sdk = Sdk::locate().map_err(errors::with(errors::DEPENDENCY))?;
    let store = Store::open()?;
    let found = store.find(key)?;
    let _op = store.lock_instance(&found.id, OP_TIMEOUT)?;
    let mut inst = store.find(&found.id)?;
    runtime::reconcile(&store, &mut inst)?;
    if inst.state.expects_process() {
        return Err(errors::with(errors::CONFLICT)(anyhow::anyhow!(
            "instance {} is already {}",
            inst.name,
            inst.state.as_str()
        )));
    }
    if inst.state == State::Created {
        return Err(errors::with(errors::CONFLICT)(anyhow::anyhow!(
            "instance {} was never started; remove it and run it again",
            inst.name
        )));
    }
    runtime::install_interrupt_handler();
    {
        let _create = store
            .lock_create()
            .map_err(errors::with(errors::CONFLICT))?;
        inst.console_port =
            runtime::allocate_port(&store, None).map_err(errors::with(errors::CONFLICT))?;
        inst.set_state(State::Starting)?;
        inst.snapshot = None;
        inst.pid = Some(
            runtime::spawn_emulator(&sdk, &store, &inst, &BootMode::Cold)
                .map_err(errors::with(errors::STARTUP))?,
        );
        store.save(&inst)?;
    }
    eprintln!("Starting {} ({}) on {}", inst.name, inst.id, inst.serial());
    let t0 = std::time::Instant::now();
    runtime::wait_ready(
        &sdk,
        &store,
        &mut inst,
        Duration::from_secs(timeout),
        runtime::interrupted,
        || eprintln!("  Android is booting..."),
    )
    .map_err(errors::with(errors::STARTUP))?;
    let secs = t0.elapsed().as_secs_f64();
    eprintln!("Ready in {secs:.1}s (cold boot)");
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&instance_json(&inst, Some(false), Some(secs), None))?
        );
    } else {
        println!("{}", inst.id);
    }
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

/// The base image a Dorkerfile asks for (`FROM`), so `dorker pull` can install it.
fn image_from_dorkerfile(file: Option<&Path>) -> Result<String> {
    let cfg = errors::with(errors::CONFIG);
    let path = file.map_or_else(|| PathBuf::from("Dorkerfile"), Path::to_path_buf);
    let source = std::fs::read_to_string(&path).map_err(|e| {
        cfg(anyhow::anyhow!(
            "no image given and could not read {}: {e}\n\nName an image (`dorker pull android:35`) or run this from a folder with a Dorkerfile (or pass -f).",
            path.display()
        ))
    })?;
    let display = path.file_name().map_or_else(
        || "Dorkerfile".to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    let df = crate::dorkerfile::parse(&display, &source).map_err(|e| cfg(e.into()))?;
    Ok(df.from.to_string())
}

/// Install a system image with the SDK's own package tool: `android sdk install`, or the legacy
/// `sdkmanager` on older command-line tools. The new CLI accepts the SDK licence without
/// asking (verified), so `pull` prints a notice first; the legacy tool prompts by itself.
pub fn pull(image: Option<&str>, file: Option<&Path>) -> Result<i32> {
    let dep = errors::with(errors::DEPENDENCY);
    let image = match image {
        Some(i) => i.to_string(),
        None => {
            let from = image_from_dorkerfile(file)?;
            eprintln!("Dorkerfile asks for {from}");
            from
        }
    };
    let r = ImageRef::parse(&image).map_err(errors::with(errors::CONFIG))?;
    let sdk = Sdk::locate().map_err(dep)?;
    let package = crate::sdk::package_for(&r);
    if sdk.system_images().iter().any(|i| i.package == package) {
        println!("{package} is already installed");
        return Ok(0);
    }
    let Some(installer) = sdk.package_installer() else {
        return Err(dep(anyhow::anyhow!(
            "no SDK package tool found under {}/cmdline-tools (looked for `android` and `sdkmanager`)\n\nInstall the Android command-line tools:\n  brew install --cask android-commandlinetools",
            sdk.root.display()
        )));
    };
    // Verified: Google's `android` CLI accepts the SDK licence by itself (it writes
    // licenses/android-sdk-license without asking, even with no terminal), so say so
    // before it happens instead of doing it silently on the user's behalf.
    if matches!(installer, crate::sdk::Installer::AndroidCli(_))
        && !sdk
            .root
            .join("licenses")
            .join("android-sdk-license")
            .is_file()
    {
        eprintln!(
            "note: Google's `android` CLI accepts the Android SDK licences on your behalf when it installs packages\n      (for arm64 images that includes the ARM binary-translation licence).\n      Read them first: https://developer.android.com/studio/terms (Ctrl-C now to stop)."
        );
    }
    eprintln!(
        "Installing {package} with {} ({})",
        installer.name(),
        installer.path().display()
    );
    let status = installer
        .command(&sdk.root, &package)
        .status()
        .with_context(|| format!("running {}", installer.path().display()))?;
    if !status.success() {
        return Err(dep(anyhow::anyhow!(
            "{} failed ({status}). Is the package name valid for this host? List what exists with `android sdk list --all`",
            installer.name()
        )));
    }
    // The SDK tools exit 0 even for a package that does not exist ("Package ... not found."),
    // so success is only trusted once the image is really on disk.
    if !sdk.system_images().iter().any(|i| i.package == package) {
        return Err(dep(anyhow::anyhow!(
            "{package} is not installed after {} finished; it probably does not exist for this host.\nList what is available with: android sdk list --all",
            installer.name()
        )));
    }
    println!("{package} installed");
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
    for (pid, port) in runtime::orphans(&store) {
        eprintln!(
            "warning: emulator pid {pid} (console port {port}) belongs to no dorker instance; stop it with `kill {pid}`"
        );
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

pub fn exec(key: &str, command: &[String], json: bool) -> Result<i32> {
    let (sdk, inst) = ready_instance(key)?;
    let mut cmd = runtime::adb(&sdk, &inst.serial());
    cmd.args(["shell", &command.join(" ")]);
    if json {
        let out = cmd.output().context("running adb")?;
        let code = code_of(out.status);
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "exit_code": code,
                "stdout": String::from_utf8_lossy(&out.stdout),
                "stderr": String::from_utf8_lossy(&out.stderr),
            }))?
        );
        return Ok(code);
    }
    Ok(code_of(cmd.status().context("running adb")?))
}

pub fn install(key: &str, apk: &Path, json: bool) -> Result<i32> {
    if !apk.is_file() {
        bail!("APK not found: {}", apk.display());
    }
    let (sdk, inst) = ready_instance(key)?;
    let mut cmd = runtime::adb(&sdk, &inst.serial());
    cmd.args(["install", "-r"]).arg(apk);
    if json {
        let out = cmd.output().context("running adb")?;
        let ok =
            out.status.success() && !String::from_utf8_lossy(&out.stdout).contains("Failure [");
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "ok": ok,
                "output": format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)).trim(),
            }))?
        );
        return Ok(if ok { 0 } else { errors::ADB });
    }
    let status = cmd.status().context("running adb")?;
    Ok(if status.success() { 0 } else { errors::ADB })
}

/// True when a raw `screencap` frame (header + RGBA pixels) is essentially one flat colour,
/// which is what a screen that has not drawn anything yet looks like. A frame counts as blank
/// when its most common colour covers at least 99.5% of the pixels; a real UI (even a mostly
/// white one with a toolbar and text) stays well below that, and one with many distinct
/// colours (wallpaper, photos) is rejected early.
pub fn frame_is_blank(raw: &[u8]) -> bool {
    if raw.len() < 12 {
        return false;
    }
    let w = u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]) as usize;
    let h = u32::from_le_bytes([raw[4], raw[5], raw[6], raw[7]]) as usize;
    let pixel_bytes = w.saturating_mul(h).saturating_mul(4);
    if w == 0 || h == 0 || raw.len() < pixel_bytes {
        return false; // unexpected layout: never block on a heuristic
    }
    // The header size varies between Android versions; the pixels are the trailing bytes.
    let pixels = &raw[raw.len() - pixel_bytes..];
    let mut counts: std::collections::HashMap<[u8; 4], usize> = std::collections::HashMap::new();
    for p in pixels.chunks_exact(4) {
        *counts.entry([p[0], p[1], p[2], p[3]]).or_insert(0) += 1;
        if counts.len() > 64 {
            return false; // gradients, wallpapers, photos: clearly drawn
        }
    }
    let total = pixels.len() / 4;
    let dominant = counts.values().copied().max().unwrap_or(0);
    dominant * 1000 >= total * 995
}

pub fn screenshot(key: &str, output: &Path, json: bool, wait_secs: u64) -> Result<i32> {
    let (sdk, inst) = ready_instance(key)?;
    // After a cold boot, READY can arrive several seconds before the launcher draws, and an
    // app can still be starting. Wait (bounded) for something other than a flat frame.
    if wait_secs > 0 {
        let deadline = std::time::Instant::now() + Duration::from_secs(wait_secs);
        loop {
            let raw = runtime::adb(&sdk, &inst.serial())
                .args(["exec-out", "screencap"])
                .output();
            let blank = raw
                .as_ref()
                .is_ok_and(|o| o.status.success() && frame_is_blank(&o.stdout));
            if !blank {
                break;
            }
            if std::time::Instant::now() >= deadline {
                eprintln!(
                    "warning: the screen is still blank after {wait_secs}s (the launcher or app may not have drawn yet, or it really is blank); capturing anyway. Use --wait 0 to skip this check."
                );
                break;
            }
            crate::log::vlog!("screen is blank; waiting for the first frame");
            std::thread::sleep(Duration::from_millis(500));
        }
    }
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
    if output == Path::new("-") {
        // PNG bytes on stdout, e.g. `dorker screenshot t1 - > shot.png`. The MCP server uses
        // this so it never has to create a temporary file.
        use std::io::{IsTerminal, Write};
        if json {
            bail!("--json cannot be combined with writing the PNG to stdout (`-`)");
        }
        if std::io::stdout().is_terminal() {
            bail!(
                "refusing to write binary image data to a terminal; redirect stdout or give a file name"
            );
        }
        let mut so = std::io::stdout().lock();
        so.write_all(&out.stdout)
            .context("writing the PNG to stdout")?;
        so.flush()?;
        eprintln!("Wrote {} bytes to stdout", out.stdout.len());
        return Ok(0);
    }
    std::fs::write(output, &out.stdout).with_context(|| format!("writing {}", output.display()))?;
    eprintln!("Saved {} ({} bytes)", output.display(), out.stdout.len());
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(
                &serde_json::json!({ "path": output, "bytes": out.stdout.len() })
            )?
        );
    }
    Ok(0)
}

pub fn logs(key: &str, follow: bool, tail: Option<u32>) -> Result<i32> {
    let (sdk, inst) = ready_instance(key)?;
    let mut cmd = runtime::adb(&sdk, &inst.serial());
    cmd.arg("logcat");
    if !follow {
        cmd.arg("-d");
    }
    if let Some(n) = tail {
        // `-t N` prints the last N lines and exits.
        cmd.args(["-t", &n.to_string()]);
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
    let store = Store::open()?;
    let inst = store.find(key)?;
    let sdk = Sdk::locate().map_err(errors::with(errors::DEPENDENCY))?;
    let _op = store.lock_instance(&inst.id, OP_TIMEOUT)?;
    let mut inst = store.find(&inst.id)?; // re-read under the lock
    stop_instance(&sdk, &store, &mut inst)?;
    println!("{}", inst.name);
    Ok(0)
}

pub fn rm(key: &str, force: bool) -> Result<i32> {
    let store = Store::open()?;
    let inst = store.find(key)?;
    let _op = store.lock_instance(&inst.id, OP_TIMEOUT)?;
    let mut inst = store.find(&inst.id)?; // re-read under the lock
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A raw screencap frame: 12-byte header (w, h, format) plus RGBA pixels.
    fn frame(w: u32, h: u32, pixel: impl Fn(usize) -> [u8; 4]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&w.to_le_bytes());
        v.extend_from_slice(&h.to_le_bytes());
        v.extend_from_slice(&1u32.to_le_bytes());
        for i in 0..(w * h) as usize {
            v.extend_from_slice(&pixel(i));
        }
        v
    }

    #[test]
    fn flat_frames_are_blank_and_real_ui_is_not() {
        assert!(
            frame_is_blank(&frame(100, 200, |_| [0, 0, 0, 255])),
            "all black"
        );
        assert!(
            frame_is_blank(&frame(100, 200, |_| [250, 250, 250, 255])),
            "all white"
        );
        // A mostly white page with a dark toolbar and some text (about 10% non-dominant).
        assert!(!frame_is_blank(&frame(100, 200, |i| if i % 10 == 0 {
            [10, 10, 10, 255]
        } else {
            [250, 250, 250, 255]
        })));
        // A launcher-like wallpaper gradient.
        assert!(!frame_is_blank(&frame(100, 200, |i| [
            (i % 251) as u8,
            (i % 199) as u8,
            80,
            255
        ])));
        // 0.3% of pixels differ: still blank (e.g. just a clock). 2% differ: content.
        assert!(frame_is_blank(&frame(100, 200, |i| if i < 60 {
            [9, 9, 9, 255]
        } else {
            [0, 0, 0, 255]
        })));
        assert!(!frame_is_blank(&frame(100, 200, |i| if i < 400 {
            [9, 9, 9, 255]
        } else {
            [0, 0, 0, 255]
        })));
    }

    #[test]
    fn unexpected_frames_never_block() {
        assert!(!frame_is_blank(&[]));
        assert!(!frame_is_blank(&[1, 2, 3]));
        // Header claims more pixels than were sent: do not guess.
        let mut short = frame(100, 200, |_| [0, 0, 0, 255]);
        short.truncate(100);
        assert!(!frame_is_blank(&short));
        // A different header length (newer Android) is handled by taking the trailing pixels.
        let mut v = frame(50, 50, |_| [7, 7, 7, 255]);
        v.splice(12..12, [0u8; 4]);
        assert!(frame_is_blank(&v));
    }
}
