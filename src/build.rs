//! `dorker build`: Dorkerfile -> immutable image.

use crate::dorkerfile::{self, Cmd, Dorkerfile};
use crate::errors;
use crate::images::{self, ImageMeta};
use crate::runtime::{self, BootMode};
use crate::sdk::Sdk;
use crate::store::{Instance, State, Store, new_id, now_secs};
use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long to let Android flush lazily persisted state before rebooting.
const FLUSH_DELAY: Duration = Duration::from_secs(6);

pub struct BuildArgs {
    pub file: Option<PathBuf>,
    pub tag: Option<String>,
    pub context: PathBuf,
    pub timeout: u64,
    pub no_cache: bool,
    /// Install a missing system image without asking (`--pull`).
    pub pull: bool,
    /// Allow `PROVISION` scripts to run without asking (`--allow-provision`).
    pub allow_provision: bool,
}

/// Decide whether the Dorkerfile's `PROVISION` scripts may run on this machine. They are
/// arbitrary host code, so, like a Makefile target from a repository you have just cloned,
/// they need consent: `--allow-provision` or `DORKER_ALLOW_PROVISION=1`, or a yes at the
/// prompt on a terminal. Anything that cannot be asked (a script, CI, the MCP server) is
/// refused unless the flag was given.
/// `scripts` are the lines to show, such as `FETCH ./fetch.sh` or `PROVISION ./setup.sh`.
fn authorize_scripts(scripts: &[String], flag: bool) -> Result<()> {
    use std::io::{BufRead, IsTerminal, Write};
    let env_ok = std::env::var("DORKER_ALLOW_PROVISION").is_ok_and(|v| !v.is_empty() && v != "0");
    if flag || env_ok {
        return Ok(());
    }
    let list = scripts
        .iter()
        .map(|s| format!("  {s}"))
        .collect::<Vec<_>>()
        .join("\n");
    let cfg = errors::with(errors::CONFIG);
    if !(std::io::stdin().is_terminal() && std::io::stderr().is_terminal()) {
        return Err(cfg(anyhow::anyhow!(
            "this Dorkerfile runs scripts on your machine while building:\n{list}\n\n\
             Read them, then re-run with --allow-provision (or set DORKER_ALLOW_PROVISION=1) to allow it."
        )));
    }
    eprintln!("This Dorkerfile runs scripts on your machine while building:\n{list}");
    for _ in 0..3 {
        eprint!("Run them now? [y/N] ");
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        if std::io::stdin().lock().read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        match line.trim().to_ascii_lowercase().as_str() {
            "y" | "yes" => return Ok(()),
            "" | "n" | "no" => break,
            _ => {}
        }
    }
    Err(cfg(anyhow::anyhow!(
        "not running the scripts; the image was not built"
    )))
}

pub struct ResolvedApk {
    pub written: String,
    pub path: PathBuf,
    pub sha256: String,
}

pub fn sha256_file(path: &Path) -> Result<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex(&h.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Resolve a path from the Dorkerfile against the build context, refusing
/// anything that escapes it (`..`, absolute paths elsewhere, symlinks out).
pub fn resolve_in_context(context: &Path, written: &str) -> Result<PathBuf> {
    resolve_directive_path(context, written, "INSTALL")
}

/// Like `resolve_in_context`, naming the directive (`INSTALL`, `PROVISION`) in errors.
fn resolve_directive_path(context: &Path, written: &str, directive: &str) -> Result<PathBuf> {
    let joined = context.join(written);
    let real = joined.canonicalize().with_context(|| {
        format!(
            "{directive} path not found: {written} (looked for {})",
            joined.display()
        )
    })?;
    if !real.starts_with(context) {
        bail!(
            "{directive} path {written:?} escapes the build context {}",
            context.display()
        );
    }
    if !real.is_file() {
        bail!("{directive} path {written:?} is not a file");
    }
    Ok(real)
}

/// What `build_new` provisions the build emulator with.
struct Provisioning<'a> {
    apks: &'a [ResolvedApk],
    scripts: &'a [ResolvedApk],
    /// The build context folder (where PROVISION scripts run).
    context: &'a Path,
}

pub struct Inputs<'a> {
    pub df: &'a Dorkerfile,
    pub system_image: &'a str,
    pub system_image_revision: &'a str,
    pub abi: &'a str,
    pub flavour: &'a str,
    pub emulator_version: &'a str,
    pub apks: &'a [ResolvedApk],
    /// `PROVISION` scripts (their content hashes are part of the cache key).
    pub provisions: &'a [ResolvedApk],
}

/// Everything that determines the built artifact. APK *paths* are excluded;
/// only their content hashes count, so moving a project does not bust the cache.
pub fn fingerprint(i: &Inputs) -> String {
    let d = i.df;
    let mut s = String::from("dorker-build-v3\n");
    let _ = writeln!(
        s,
        "system_image={}@{}",
        i.system_image, i.system_image_revision
    );
    let _ = writeln!(s, "emulator={}", i.emulator_version);
    let _ = writeln!(s, "abi={}", i.abi);
    let _ = writeln!(
        s,
        "memory={} cpus={} disk={}",
        d.memory_mb, d.cpus, d.disk_mb
    );
    let _ = writeln!(
        s,
        "device={} res={}x{} headless={} gpu={}",
        d.device,
        d.resolution.0,
        d.resolution.1,
        d.headless,
        d.gpu_mode()
    );
    for a in i.apks {
        let _ = writeln!(s, "apk={}", a.sha256);
    }
    // Only written when used, so images built before PROVISION existed keep their ids.
    for p in i.provisions {
        let _ = writeln!(s, "provision={}", p.sha256);
    }
    for st in &d.settings {
        let _ = writeln!(s, "setting={} {} {}", st.namespace, st.key, st.value);
    }
    let _ = writeln!(s, "snapshot={}", d.snapshot.as_deref().unwrap_or("-"));
    let _ = writeln!(s, "flavour={}", i.flavour);
    hex(&Sha256::digest(s.as_bytes()))
}

fn render_lock(i: &Inputs, fp: &str) -> String {
    let d = i.df;
    let mut s = String::new();
    let _ = writeln!(
        s,
        "# Generated by dorker build. Commit this file to pin the environment."
    );
    let _ = writeln!(s, "version = 1\nfingerprint = \"{fp}\"\n");
    let _ = writeln!(
        s,
        "[android]\napi = {}\nflavour = \"{}\"\nabi = \"{}\"",
        d.from.api, i.flavour, i.abi
    );
    let _ = writeln!(
        s,
        "system_image = \"{}\"\nsystem_image_revision = \"{}\"\n",
        i.system_image, i.system_image_revision
    );
    let _ = writeln!(s, "[emulator]\nversion = \"{}\"\n", i.emulator_version);
    let _ = writeln!(
        s,
        "[device]\nprofile = \"{}\"\nresolution = \"{}x{}\"",
        d.device, d.resolution.0, d.resolution.1
    );
    let _ = writeln!(
        s,
        "memory_mb = {}\ncpus = {}\ndisk_mb = {}\nheadless = {}\ngpu = \"{}\"",
        d.memory_mb,
        d.cpus,
        d.disk_mb,
        d.headless,
        d.gpu_mode()
    );
    if let Some(n) = &d.snapshot {
        let _ = writeln!(s, "snapshot = \"{n}\"");
    }
    for a in i.apks {
        let _ = writeln!(
            s,
            "\n[[application]]\npath = \"{}\"\nsha256 = \"{}\"",
            a.written, a.sha256
        );
    }
    for p in i.provisions {
        let _ = writeln!(
            s,
            "\n[[provision]]\npath = \"{}\"\nsha256 = \"{}\"",
            p.written, p.sha256
        );
    }
    s
}

/// Value of `key = "value"` within `[section]` of a lockfile (top level = "").
fn lock_value(lock: &str, section: &str, key: &str) -> Option<String> {
    let mut current = String::new();
    for l in lock.lines() {
        let l = l.trim();
        if let Some(sec) = l.strip_prefix('[') {
            current = sec.trim_matches(|c| c == '[' || c == ']').to_string();
        } else if let Some((k, v)) = l.split_once('=') {
            if current == section && k.trim() == key {
                return Some(v.trim().trim_matches('"').to_string());
            }
        }
    }
    None
}

fn warn_on_drift(lock_path: &Path, i: &Inputs) {
    let Ok(old) = std::fs::read_to_string(lock_path) else {
        return;
    };
    for (section, key, now) in [
        ("android", "system_image_revision", i.system_image_revision),
        ("emulator", "version", i.emulator_version),
    ] {
        if let Some(was) = lock_value(&old, section, key) {
            if was != now {
                eprintln!(
                    "warning: {key} changed since Dorkerfile.lock was written ({was} -> {now}); updating the lockfile"
                );
            }
        }
    }
}

pub fn build(a: BuildArgs) -> Result<i32> {
    let meta = build_image(&a)?;
    println!("{}", meta.id);
    Ok(0)
}

/// Build (or find in the cache) the image a Dorkerfile describes. Progress goes to stderr and
/// nothing is printed to stdout, so callers such as `dorker run` can use it quietly.
pub fn build_image(a: &BuildArgs) -> Result<ImageMeta> {
    let context = a
        .context
        .canonicalize()
        .with_context(|| format!("build context not found: {}", a.context.display()))?;
    let file = a.file.clone().unwrap_or_else(|| context.join("Dorkerfile"));
    let source =
        std::fs::read_to_string(&file).with_context(|| format!("reading {}", file.display()))?;
    let display = file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Dorkerfile".into());
    let df =
        dorkerfile::parse(&display, &source).map_err(|e| errors::with(errors::CONFIG)(e.into()))?;
    let tag = a.tag.as_deref().map(images::normalize_tag).transpose()?;
    // FETCH scripts come first: INSTALL files must exist before they can be hashed.
    let authorized = run_fetches(&df, &context, a.allow_provision)?;

    let sdk = Sdk::locate().map_err(errors::with(errors::DEPENDENCY))?;
    let sys = crate::commands::resolve_or_pull(
        &sdk,
        &df.from,
        crate::commands::PullPolicy::from_flag(a.pull),
    )
    .map_err(errors::with(errors::DEPENDENCY))?;
    let revision = sdk.system_image_revision(&sys)?;
    let emulator_version = sdk.emulator_version()?;
    let host_cpus = std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(1);
    if df.cpus > host_cpus {
        bail!(
            "CPUS {} exceeds the {host_cpus} cores available on this host",
            df.cpus
        );
    }

    let apks: Vec<ResolvedApk> = df
        .installs
        .iter()
        .map(|w| {
            let path = resolve_in_context(&context, w).map_err(errors::with(errors::CONFIG))?;
            let sha256 = sha256_file(&path)?;
            Ok(ResolvedApk {
                written: w.clone(),
                path,
                sha256,
            })
        })
        .collect::<Result<_>>()?;

    // PROVISION scripts run on the host, so they must be executable.
    let provisions: Vec<ResolvedApk> = df
        .provisions
        .iter()
        .map(|w| {
            let path = resolve_directive_path(&context, w, "PROVISION")
                .map_err(errors::with(errors::CONFIG))?;
            require_executable(&path, w, "PROVISION").map_err(errors::with(errors::CONFIG))?;
            let sha256 = sha256_file(&path)?;
            Ok(ResolvedApk {
                written: w.clone(),
                path,
                sha256,
            })
        })
        .collect::<Result<_>>()?;

    let inputs = Inputs {
        df: &df,
        system_image: &sys.package,
        system_image_revision: &revision,
        abi: &sys.abi,
        flavour: &sys.flavour,
        emulator_version: &emulator_version,
        apks: &apks,
        provisions: &provisions,
    };
    let fp = fingerprint(&inputs);
    crate::log::vlog!(
        "build fingerprint {fp} (system image {}@{}, emulator {}, {} apk(s), gpu {})",
        sys.package,
        revision,
        emulator_version,
        apks.len(),
        df.gpu_mode()
    );
    let id = fp[..12].to_string();
    let store = Store::open()?;
    let lock_path = file.parent().unwrap_or(&context).join("Dorkerfile.lock");
    warn_on_drift(&lock_path, &inputs);

    let launch = df.cmd.as_ref().map(|Cmd::Launch(p)| p.clone());
    let mut meta = ImageMeta {
        id: id.clone(),
        fingerprint: fp.clone(),
        tags: vec![],
        api: sys.api,
        flavour: sys.flavour.clone(),
        abi: sys.abi.clone(),
        system_image: sys.package.clone(),
        system_image_revision: revision.clone(),
        emulator_version: emulator_version.clone(),
        memory_mb: df.memory_mb,
        cpus: df.cpus,
        disk_mb: df.disk_mb,
        device: df.device.clone(),
        width: df.resolution.0,
        height: df.resolution.1,
        headless: df.headless,
        gpu: df.gpu_mode(),
        snapshot: df.snapshot.clone(),
        launch,
        created_at: now_secs(),
        last_used: None,
    };

    let cached = !a.no_cache && images::image_dir(&store, &id).join("image.json").is_file();
    crate::log::vlog!(
        "cache {} for image {id} (--no-cache: {})",
        if cached { "hit" } else { "miss" },
        a.no_cache
    );
    if cached {
        meta = images::find(&store, &id)?;
        images::touch_used(&store, &id);
        eprintln!("Using cached image {id} (identical Dorkerfile, APKs and toolchain)");
    } else {
        if !df.provisions.is_empty() {
            let lines: Vec<String> = df
                .provisions
                .iter()
                .map(|p| format!("PROVISION {p}"))
                .collect();
            authorize_scripts(&lines, authorized)?;
        }
        // A build holds the guest's RAM image (the snapshot) plus provisioned userdata.
        runtime::ensure_free_space(&store.root(), df.memory_mb * 1024 * 1024, "build an image")?;
        runtime::install_interrupt_handler();
        build_new(
            &sdk,
            &store,
            &sys,
            &df,
            &Provisioning {
                apks: &apks,
                scripts: &provisions,
                context: &context,
            },
            &mut meta,
            a.timeout,
        )
        .map_err(errors::with(errors::BUILD))?;
    }
    if let Some(t) = &tag {
        images::apply_tag(&store, &mut meta, t)?;
    }
    std::fs::write(&lock_path, render_lock(&inputs, &fp))
        .with_context(|| format!("writing {}", lock_path.display()))?;
    eprintln!("Wrote {}", lock_path.display());
    Ok(meta)
}

fn build_new(
    sdk: &Sdk,
    store: &Store,
    sys: &crate::sdk::SystemImage,
    df: &Dorkerfile,
    prov: &Provisioning,
    meta: &mut ImageMeta,
    timeout: u64,
) -> Result<()> {
    // A transient instance doubles as the build workspace, so interrupted builds
    // show up in `dorker ps -a` and are cleaned by `dorker rm`.
    let lock = store.lock_create()?;
    let bid = new_id();
    let name = format!("build-{}", meta.id);
    if store
        .load_all()?
        .iter()
        .any(|i| i.name == name && i.state.expects_process())
    {
        return Err(errors::with(errors::CONFLICT)(anyhow::anyhow!(
            "an identical build ({name}) is already running"
        )));
    }
    if let Ok(old) = store.find(&name) {
        let _ = store.remove(&old); // leftover from an earlier failed build
    }
    let port = runtime::allocate_port(store, None)?;
    let mut inst = Instance {
        id: bid,
        name,
        image: format!("build:{}", meta.id),
        api: sys.api,
        flavour: sys.flavour.clone(),
        abi: sys.abi.clone(),
        memory_mb: df.memory_mb,
        cpus: df.cpus,
        disk_mb: df.disk_mb,
        headless: df.headless,
        auto_remove: false,
        console_port: port,
        pid: None,
        state: State::Created,
        created_at: now_secs(),
        device: df.device.clone(),
        width: df.resolution.0,
        height: df.resolution.1,
        gpu: df.gpu_mode(),
        image_id: None,
        snapshot: None,
        owner: None,
    };
    store.save(&inst)?;
    let started = (|| -> Result<()> {
        runtime::write_avd(store, &inst, sys)?;
        inst.set_state(State::Starting)?;
        inst.pid = Some(runtime::spawn_emulator(
            sdk,
            store,
            &inst,
            &BootMode::ColdSaveable,
        )?);
        store.save(&inst)
    })();
    drop(lock);

    let result = started.and_then(|()| provision(sdk, store, &mut inst, df, prov, timeout));
    // Always stop the build emulator, whatever happened.
    runtime::kill_instance(sdk, &inst, Duration::from_secs(20));
    let published = result.and_then(|()| publish(store, &inst, meta));
    let _ = store.remove(&inst);
    published
}

pub(crate) fn adb_checked(sdk: &Sdk, serial: &str, args: &[&str], what: &str) -> Result<String> {
    let out = runtime::adb(sdk, serial)
        .args(args)
        .output()
        .with_context(|| format!("running adb for {what}"))?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    if !out.status.success() || text.contains("Failure [") {
        bail!("{what} failed:\n{}", text.trim());
    }
    Ok(text)
}

/// PROVISION scripts are run directly, so they need the executable bit.
fn require_executable(path: &Path, written: &str, label: &str) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if std::fs::metadata(path)?.permissions().mode() & 0o111 == 0 {
            bail!("{label} script {written:?} is not executable; run `chmod +x {written}`");
        }
    }
    #[cfg(not(unix))]
    let _ = (path, written);
    Ok(())
}

/// How long one PROVISION script may run before the build gives up on it.
const PROVISION_TIMEOUT: Duration = Duration::from_secs(600);

/// Run a PROVISION script on the host against the build emulator. It gets the adb serial as
/// its only argument and in `ANDROID_SERIAL` / `DORKER_SERIAL`, the adb path in `ADB`, and
/// runs in the build context folder. Its output goes to stderr (stdout carries the image id).
fn run_provision(sdk: &Sdk, serial: &str, script: &ResolvedApk, context: &Path) -> Result<()> {
    run_script("PROVISION", script, context, |c| {
        c.arg(serial)
            .env("ANDROID_SERIAL", serial)
            .env("DORKER_SERIAL", serial)
            .env("ADB", sdk.adb());
    })
}

/// Run a host script from the Dorkerfile in the build context folder, with its stdout sent to
/// stderr (stdout carries the image id). `configure` adds arguments and environment.
fn run_script(
    label: &str,
    script: &ResolvedApk,
    context: &Path,
    configure: impl FnOnce(&mut std::process::Command),
) -> Result<()> {
    use std::process::{Command, Stdio};
    let mut cmd = Command::new(&script.path);
    cmd.current_dir(context)
        .stdin(Stdio::null())
        .stdout(Stdio::from(std::io::stderr()))
        .stderr(Stdio::inherit());
    configure(&mut cmd);
    let mut child = cmd
        .spawn()
        .with_context(|| format!("starting {label} script {}", script.written))?;
    let started = std::time::Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            if status.success() {
                return Ok(());
            }
            bail!("{label} script {} failed ({status})", script.written);
        }
        if runtime::interrupted() {
            let _ = child.kill();
            let _ = child.wait();
            bail!("interrupted while running {}", script.written);
        }
        if started.elapsed() > PROVISION_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            bail!(
                "{label} script {} did not finish within {} s",
                script.written,
                PROVISION_TIMEOUT.as_secs()
            );
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// `FETCH` scripts create `INSTALL` files that are missing (for example by downloading them).
/// They run only when a file is missing, so a cached build downloads nothing and a build stays
/// reproducible; delete the file, or run the script yourself, to refresh it. Returns whether
/// the user has already agreed to run the Dorkerfile's scripts.
fn run_fetches(df: &Dorkerfile, context: &Path, flag: bool) -> Result<bool> {
    let missing = |df: &Dorkerfile| -> Vec<String> {
        df.installs
            .iter()
            .filter(|w| !context.join(w).exists())
            .cloned()
            .collect()
    };
    let wanted = missing(df);
    if wanted.is_empty() || df.fetches.is_empty() {
        return Ok(flag);
    }
    // Ask once for everything that will run, the PROVISION scripts included.
    let mut lines: Vec<String> = df.fetches.iter().map(|f| format!("FETCH {f}")).collect();
    lines.extend(df.provisions.iter().map(|p| format!("PROVISION {p}")));
    authorize_scripts(&lines, flag)?;
    let cfg = errors::with(errors::CONFIG);
    for written in &df.fetches {
        let path = resolve_directive_path(context, written, "FETCH").map_err(cfg)?;
        require_executable(&path, written, "FETCH").map_err(cfg)?;
        eprintln!(
            "Fetching missing files ({}) with {written}",
            wanted.join(", ")
        );
        let script = ResolvedApk {
            written: written.clone(),
            path,
            sha256: String::new(),
        };
        run_script("FETCH", &script, context, |_| {}).map_err(errors::with(errors::BUILD))?;
    }
    let still = missing(df);
    if !still.is_empty() {
        return Err(cfg(anyhow::anyhow!(
            "the FETCH script(s) ran but INSTALL file(s) are still missing: {}",
            still.join(", ")
        )));
    }
    Ok(true)
}

fn provision(
    sdk: &Sdk,
    store: &Store,
    inst: &mut Instance,
    df: &Dorkerfile,
    prov: &Provisioning,
    timeout: u64,
) -> Result<()> {
    let (apks, provisions, context) = (prov.apks, prov.scripts, prov.context);
    // Number the steps so a long build shows how far along it is.
    let persists = !apks.is_empty() || !df.settings.is_empty();
    let total = 1
        + apks.len()
        + usize::from(!df.settings.is_empty())
        + provisions.len()
        + usize::from(persists)
        + usize::from(df.snapshot.is_some());
    let st = crate::style::Style::stderr();
    let mut n = 0;
    let mut step = |text: String| {
        n += 1;
        eprintln!("{} {text}", st.cyan(&format!("[{n}/{total}]")));
    };
    step(format!("Booting build instance ({})...", inst.serial()));
    runtime::wait_ready(
        sdk,
        store,
        inst,
        Duration::from_secs(timeout),
        runtime::interrupted,
        || {},
    )?;
    let serial = inst.serial();
    for apk in apks {
        step(format!("Installing {}", apk.written));
        crate::xapk::install(sdk, &serial, &apk.path, &apk.written)?;
    }
    if !df.settings.is_empty() {
        let keys: Vec<&str> = df.settings.iter().map(|s| s.key.as_str()).collect();
        step(format!(
            "Applying {} setting{} ({})",
            keys.len(),
            if keys.len() == 1 { "" } else { "s" },
            keys.join(", ")
        ));
    }
    for s in &df.settings {
        adb_checked(
            sdk,
            &serial,
            &["shell", "settings", "put", &s.namespace, &s.key, &s.value],
            &format!("setting {}", s.key),
        )?;
    }
    if persists {
        // Android persists settings and the package database lazily, and an adb
        // reboot is abrupt, so give it time to flush, reboot, then prove the state
        // survived. Otherwise a cold boot of the image would silently lose it.
        step("Persisting provisioned state: flush, reboot, verify (about 40s)".to_string());
        let flush_until = std::time::Instant::now() + FLUSH_DELAY;
        while std::time::Instant::now() < flush_until && !runtime::interrupted() {
            std::thread::sleep(Duration::from_millis(250));
        }
        runtime::reboot_and_wait(sdk, &serial, Duration::from_secs(timeout))?;
        for s in &df.settings {
            let out = adb_checked(
                sdk,
                &serial,
                &["shell", "settings", "get", &s.namespace, &s.key],
                &format!("reading setting {}", s.key),
            )?;
            if out.trim() != s.value {
                bail!(
                    "setting {} {} did not persist across a reboot (expected {:?}, found {:?})",
                    s.namespace,
                    s.key,
                    s.value,
                    out.trim()
                );
            }
        }
    }
    // PROVISION scripts run last, after the persist reboot: they may start things that live in
    // memory (a server, an open screen) that a reboot would throw away, and the snapshot
    // below then keeps them running.
    for script in provisions {
        step(format!("Running {}", script.written));
        run_provision(sdk, &serial, script, context)?;
    }
    if !provisions.is_empty() {
        // Flush what the scripts wrote, so a cold boot of the image has it too.
        adb_checked(sdk, &serial, &["shell", "sync"], "flushing to disk")?;
    }
    if let Some(name) = &df.snapshot {
        step(format!("Saving snapshot {name:?}"));
        let out = adb_checked(
            sdk,
            &serial,
            &["emu", "avd", "snapshot", "save", name],
            "saving snapshot",
        )?;
        if out.lines().any(|l| l.starts_with("KO")) {
            bail!("emulator refused to save snapshot {name:?}: {}", out.trim());
        }
        let dir = store
            .instance_dir(&inst.id)
            .join("avd")
            .join(format!("{}.avd", runtime::AVD_NAME))
            .join("snapshots")
            .join(name);
        if !dir.is_dir() {
            bail!(
                "snapshot {name:?} was not created (expected {})",
                dir.display()
            );
        }
    }
    Ok(())
}

/// Move the build workspace into the image store atomically.
fn publish(store: &Store, inst: &Instance, meta: &mut ImageMeta) -> Result<()> {
    let final_dir = images::image_dir(store, &meta.id);
    let partial = store.images_dir().join(format!(".{}.partial", meta.id));
    let _ = std::fs::remove_dir_all(&partial);
    std::fs::create_dir_all(&partial)?;
    let inst_dir = store.instance_dir(&inst.id);
    std::fs::rename(inst_dir.join("avd"), partial.join("avd"))?;
    let _ = std::fs::copy(inst_dir.join("emulator.log"), partial.join("build.log"));
    std::fs::write(
        partial.join("image.json"),
        serde_json::to_vec_pretty(&*meta)?,
    )?;
    if final_dir.exists() {
        // Rebuild with --no-cache: replace atomically-ish, keeping existing tags.
        if let Ok(old) = images::find(store, &meta.id) {
            meta.tags = old.tags;
            std::fs::write(
                partial.join("image.json"),
                serde_json::to_vec_pretty(&*meta)?,
            )?;
        }
        std::fs::remove_dir_all(&final_dir)?;
    }
    std::fs::rename(&partial, &final_dir)?;
    eprintln!("Built image {}", meta.id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Dir;

    fn inputs<'a>(
        df: &'a Dorkerfile,
        apks: &'a [ResolvedApk],
        rev: &'a str,
        emu: &'a str,
    ) -> Inputs<'a> {
        Inputs {
            df,
            system_image: "system-images;android-35;google_apis;arm64-v8a",
            system_image_revision: rev,
            abi: "arm64-v8a",
            flavour: "google_apis",
            emulator_version: emu,
            apks,
            provisions: &[],
        }
    }

    fn apk(path: &str, sha: &str) -> ResolvedApk {
        ResolvedApk {
            written: path.into(),
            path: path.into(),
            sha256: sha.into(),
        }
    }

    #[test]
    fn fingerprint_tracks_inputs() {
        let df = dorkerfile::parse("D", "FROM android:35\nSETTING a 1\nSNAPSHOT ready\n").unwrap();
        let a = [apk("./a.apk", "aa")];
        let base = fingerprint(&inputs(&df, &a, "9", "1.0"));
        assert_eq!(
            base,
            fingerprint(&inputs(&df, &a, "9", "1.0")),
            "deterministic"
        );
        // APK path is irrelevant, its content is not.
        assert_eq!(
            base,
            fingerprint(&inputs(&df, &[apk("elsewhere/x.apk", "aa")], "9", "1.0"))
        );
        assert_ne!(
            base,
            fingerprint(&inputs(&df, &[apk("./a.apk", "bb")], "9", "1.0"))
        );
        assert_ne!(
            base,
            fingerprint(&inputs(&df, &a, "10", "1.0")),
            "system image revision"
        );
        assert_ne!(
            base,
            fingerprint(&inputs(&df, &a, "9", "2.0")),
            "emulator version"
        );
        let df2 = dorkerfile::parse("D", "FROM android:35\nSETTING a 2\nSNAPSHOT ready\n").unwrap();
        assert_ne!(base, fingerprint(&inputs(&df2, &a, "9", "1.0")), "settings");
        // The GPU mode is part of the snapshot's hardware identity, so it must bust the cache.
        let gpu = dorkerfile::parse(
            "D",
            "FROM android:35\nSETTING a 1\nSNAPSHOT ready\nGPU guest\n",
        )
        .unwrap();
        assert_ne!(base, fingerprint(&inputs(&gpu, &a, "9", "1.0")), "gpu mode");
        let df3 = dorkerfile::parse(
            "D",
            "FROM android:35\nMEMORY 3GB\nSETTING a 1\nSNAPSHOT ready\n",
        )
        .unwrap();
        assert_ne!(
            base,
            fingerprint(&inputs(&df3, &a, "9", "1.0")),
            "resources"
        );
    }

    #[test]
    fn context_paths_cannot_escape() {
        let d = Dir::new();
        let ctx = d.path().join("ctx");
        std::fs::create_dir_all(ctx.join("sub")).unwrap();
        std::fs::write(ctx.join("sub/app.apk"), b"x").unwrap();
        std::fs::write(d.path().join("secret.apk"), b"x").unwrap();
        let ctx = ctx.canonicalize().unwrap();
        assert!(resolve_in_context(&ctx, "./sub/app.apk").is_ok());
        assert!(
            resolve_in_context(&ctx, "../secret.apk")
                .unwrap_err()
                .to_string()
                .contains("escapes")
        );
        let abs = d.path().join("secret.apk");
        assert!(resolve_in_context(&ctx, &abs.to_string_lossy()).is_err());
        assert!(
            resolve_in_context(&ctx, "missing.apk")
                .unwrap_err()
                .to_string()
                .contains("not found")
        );
        assert!(
            resolve_in_context(&ctx, "sub")
                .unwrap_err()
                .to_string()
                .contains("not a file")
        );
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(d.path().join("secret.apk"), ctx.join("link.apk")).unwrap();
            assert!(
                resolve_in_context(&ctx, "link.apk").is_err(),
                "symlink out of context"
            );
        }
    }

    #[test]
    fn sha256_matches_known_vector() {
        let d = Dir::new();
        let f = d.path().join("f");
        std::fs::write(&f, b"abc").unwrap();
        assert_eq!(
            sha256_file(&f).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn lockfile_roundtrip_values() {
        let df = dorkerfile::parse("D", "FROM android:35\nINSTALL a.apk\n").unwrap();
        let a = [apk("./a.apk", "aa")];
        let lock = render_lock(&inputs(&df, &a, "9.0.0", "37.2.12.0"), "fp");
        assert_eq!(
            lock_value(&lock, "android", "system_image_revision").as_deref(),
            Some("9.0.0")
        );
        assert_eq!(
            lock_value(&lock, "emulator", "version").as_deref(),
            Some("37.2.12.0")
        );
        assert_eq!(lock_value(&lock, "", "version").as_deref(), Some("1"));
        assert_eq!(
            lock_value(&lock, "application", "sha256").as_deref(),
            Some("aa")
        );
        assert!(lock.contains("[[application]]"));
    }
}
