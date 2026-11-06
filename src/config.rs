//! The optional config file, for settings that differ between machines (where to keep the
//! images, where the SDK is, which GPU mode to use). Located at
//! `$XDG_CONFIG_HOME/dorker/config.toml`, else `~/.config/dorker/config.toml`;
//! `DORKER_CONFIG` names another file. Every setting is optional, and an environment variable
//! (`DORKER_HOME`, `ANDROID_HOME`, `DORKER_MIN_FREE_MB`) beats the file.

use crate::dorkerfile::GPU_MODES;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::path::{Path, PathBuf};
#[cfg(not(test))]
use std::sync::OnceLock;

/// The file as written. Unknown keys are an error, so a typo does not silently do nothing.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    home: Option<String>,
    sdk: Option<String>,
    gpu: Option<String>,
    min_free_mb: Option<u64>,
}

/// The validated settings.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Config {
    /// Where images and instances are kept.
    pub home: Option<PathBuf>,
    /// The Android SDK root.
    pub sdk: Option<PathBuf>,
    /// Default emulator GPU mode, for a Dorkerfile with no `GPU` line and for base images.
    pub gpu: Option<String>,
    /// Refuse to start work with less free disk than this many MB.
    pub min_free_mb: Option<u64>,
}

/// `~` and `~/x` mean the home directory. Anything else must already be absolute.
fn expand_path(raw: &str, home: Option<&Path>, key: &str) -> Result<PathBuf> {
    let raw = raw.trim();
    if raw.is_empty() {
        bail!("`{key}` is empty");
    }
    let path = if raw == "~" || raw.starts_with("~/") {
        let home = home.context("`~` needs HOME to be set")?;
        home.join(raw.trim_start_matches('~').trim_start_matches('/'))
    } else {
        PathBuf::from(raw)
    };
    if !path.is_absolute() {
        bail!("`{key}` must be an absolute path (or start with `~/`), not {raw:?}");
    }
    Ok(path)
}

/// Parse and validate the text of a config file. Pure, so it is unit-tested.
pub fn parse(text: &str, home: Option<&Path>) -> Result<Config> {
    let raw: RawFile = toml::from_str(text).map_err(|e| anyhow::anyhow!("{}", e.message()))?;
    if let Some(g) = &raw.gpu {
        if !GPU_MODES.contains(&g.as_str()) {
            bail!("`gpu` must be one of {}, not {g:?}", GPU_MODES.join(", "));
        }
    }
    if raw.min_free_mb == Some(0) {
        bail!("`min_free_mb` must be at least 1");
    }
    Ok(Config {
        home: raw
            .home
            .as_deref()
            .map(|p| expand_path(p, home, "home"))
            .transpose()?,
        sdk: raw
            .sdk
            .as_deref()
            .map(|p| expand_path(p, home, "sdk"))
            .transpose()?,
        gpu: raw.gpu,
        min_free_mb: raw.min_free_mb,
    })
}

/// Where the config file is (whether or not it exists).
pub fn path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("DORKER_CONFIG").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(p));
    }
    if let Some(x) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(x).join("dorker/config.toml"));
    }
    crate::sdk::dirs_home().map(|h| h.join(".config/dorker/config.toml"))
}

#[cfg(not(test))]
fn load() -> Result<Config> {
    let Some(path) = path() else {
        return Ok(Config::default());
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
        Err(e) => bail!("reading {}: {e}", path.display()),
    };
    parse(&text, crate::sdk::dirs_home().as_deref())
        .with_context(|| format!("invalid config file {}", path.display()))
}

#[cfg(not(test))]
static LOADED: OnceLock<std::result::Result<Config, String>> = OnceLock::new();

/// The settings from the config file (defaults when there is none). An unreadable or invalid
/// file is an error naming the file; `main` checks this once at start-up. Under `cargo test`
/// the file is never read, so a developer's own config cannot change a test's outcome.
pub fn get() -> Result<&'static Config> {
    #[cfg(test)]
    {
        static EMPTY: Config = Config {
            home: None,
            sdk: None,
            gpu: None,
            min_free_mb: None,
        };
        Ok(&EMPTY)
    }
    #[cfg(not(test))]
    LOADED
        .get_or_init(|| load().map_err(|e| format!("{e:#}")))
        .as_ref()
        .map_err(|e| anyhow::anyhow!("{e}"))
}

/// The settings, or the defaults if the file is invalid (callers that cannot fail use this;
/// the invalid file has already been reported at start-up).
pub fn current() -> Config {
    get().cloned().unwrap_or_default()
}

/// A commented example file, written by `dorker config init`.
pub const EXAMPLE: &str = r#"# dorker configuration. Every setting is optional, and an environment variable beats this file.
# Settings that differ between machines belong here, for example a Mac and a Linux box.

# Where images and instances are kept (several GB each). Default: ~/.dorker
# Also settable with DORKER_HOME. Keep the whole store on one local filesystem that can
# clone files: APFS, XFS created with reflink=1, or Btrfs. A clone shares the image's
# blocks until the instance writes. NFS and ext4 cannot clone, so every instance is a
# full copy of the image.
# home = "/mnt/big-disk/dorker"

# The Android SDK. Default: ANDROID_HOME / ANDROID_SDK_ROOT, then ~/Library/Android/sdk and
# ~/Android/Sdk.
# sdk = "~/Android/Sdk"

# Emulator GPU mode for a Dorkerfile without a `GPU` line, and for base `android:<api>` runs.
# One of: auto, swiftshader_indirect, angle_indirect, guest, host, off.
# Default: host on macOS, swiftshader_indirect elsewhere. It is part of an image's identity,
# so changing it rebuilds images.
# gpu = "swiftshader_indirect"

# Refuse to start work with less free disk than this many MB. Default: 2048.
# Also settable with DORKER_MIN_FREE_MB.
# min_free_mb = 2048
"#;

/// One effective setting: its value and where that value came from.
struct Row {
    name: &'static str,
    value: String,
    source: String,
}

fn env_set(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| !v.is_empty())
}

/// The settings dorker is using right now, and where each one comes from.
fn rows(cfg: &Config) -> Vec<Row> {
    let home_source = if env_set("DORKER_HOME") {
        "DORKER_HOME"
    } else if cfg.home.is_some() {
        "config file"
    } else {
        "default"
    };
    let home =
        crate::store::default_root().map_or_else(|e| format!("({e})"), |p| p.display().to_string());

    let (sdk, sdk_source) = match crate::sdk::Sdk::locate() {
        Ok(s) => {
            let source = ["ANDROID_HOME", "ANDROID_SDK_ROOT"]
                .into_iter()
                .find(|v| std::env::var_os(v).is_some_and(|x| Path::new(&x) == s.root))
                .map(str::to_string)
                .or_else(|| {
                    (cfg.sdk.as_deref() == Some(s.root.as_path()))
                        .then(|| "config file".to_string())
                })
                .unwrap_or_else(|| "found in a usual place".to_string());
            (s.root.display().to_string(), source)
        }
        Err(_) => (
            "(not found)".to_string(),
            "set ANDROID_HOME or `sdk` in the config".to_string(),
        ),
    };

    let (gpu, gpu_source) = match &cfg.gpu {
        Some(g) => (g.clone(), "config file".to_string()),
        None => (
            crate::dorkerfile::platform_default_gpu(true).to_string(),
            format!("default for {}", std::env::consts::OS),
        ),
    };

    let (free, free_source) = if let Some(v) = std::env::var("DORKER_MIN_FREE_MB")
        .ok()
        .filter(|v| v.trim().parse::<u64>().is_ok())
    {
        (v.trim().to_string(), "DORKER_MIN_FREE_MB")
    } else if let Some(m) = cfg.min_free_mb {
        (m.to_string(), "config file")
    } else {
        ("2048".to_string(), "default")
    };

    vec![
        Row {
            name: "home",
            value: home,
            source: home_source.into(),
        },
        Row {
            name: "sdk",
            value: sdk,
            source: sdk_source,
        },
        Row {
            name: "gpu",
            value: gpu,
            source: gpu_source,
        },
        Row {
            name: "min_free_mb",
            value: free,
            source: free_source.into(),
        },
    ]
}

/// `dorker config`: the file, and the settings in effect.
pub fn show(json: bool) -> Result<i32> {
    let cfg = get()
        .map_err(crate::errors::with(crate::errors::CONFIG))?
        .clone();
    let path = path();
    let exists = path.as_ref().is_some_and(|p| p.is_file());
    let rows = rows(&cfg);
    if json {
        let settings: serde_json::Map<String, serde_json::Value> = rows
            .iter()
            .map(|r| {
                (
                    r.name.to_string(),
                    serde_json::json!({"value": r.value, "source": r.source}),
                )
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "path": path, "exists": exists, "settings": settings,
            }))?
        );
        return Ok(0);
    }
    let st = crate::style::Style::stdout();
    match &path {
        Some(p) if exists => println!("{}  {}", st.bold("Config file"), p.display()),
        Some(p) => println!(
            "{}  {} {}",
            st.bold("Config file"),
            p.display(),
            st.dim("(none yet: `dorker config init` writes an example)")
        ),
        None => println!("{}  (HOME is not set)", st.bold("Config file")),
    }
    let w = rows.iter().map(|r| r.value.len()).max().unwrap_or(0) + 2;
    println!(
        "\n{}",
        st.dim(&format!("{:<13}{:<w$}FROM", "SETTING", "VALUE"))
    );
    for r in &rows {
        println!(
            "{}{}{}",
            st.green_bold(&format!("{:<13}", r.name)),
            r.value.clone() + &" ".repeat(w.saturating_sub(r.value.len())),
            st.dim(&r.source)
        );
    }
    Ok(0)
}

/// `dorker config path`.
pub fn print_path() -> Result<i32> {
    match path() {
        Some(p) => {
            println!("{}", p.display());
            Ok(0)
        }
        None => bail!("HOME is not set, so there is no default config path; set DORKER_CONFIG"),
    }
}

/// `dorker config init`: write the commented example file.
pub fn init(force: bool) -> Result<i32> {
    let Some(p) = path() else {
        bail!("HOME is not set; set DORKER_CONFIG to say where to write it");
    };
    if p.exists() && !force {
        return Err(crate::errors::with(crate::errors::CONFLICT)(
            anyhow::anyhow!(
                "{} already exists; edit it, or pass --force to replace it",
                p.display()
            ),
        ));
    }
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(&p, EXAMPLE).with_context(|| format!("writing {}", p.display()))?;
    println!("{}", p.display());
    eprintln!(
        "Wrote an example config with every setting commented out. Edit it, then run `dorker config` to see what is in effect."
    );
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> Option<&'static Path> {
        Some(Path::new("/home/u"))
    }

    #[test]
    fn an_empty_file_is_all_defaults() {
        assert_eq!(parse("", home()).unwrap(), Config::default());
        assert_eq!(
            parse("# only a comment\n", home()).unwrap(),
            Config::default()
        );
    }

    #[test]
    fn every_setting_parses_and_tilde_expands() {
        let c = parse(
            "home = \"~/dorker\"\nsdk = \"/opt/sdk\"\ngpu = \"host\"\nmin_free_mb = 4096 # more\n",
            home(),
        )
        .unwrap();
        assert_eq!(c.home, Some(PathBuf::from("/home/u/dorker")));
        assert_eq!(c.sdk, Some(PathBuf::from("/opt/sdk")));
        assert_eq!(c.gpu.as_deref(), Some("host"));
        assert_eq!(c.min_free_mb, Some(4096));
        assert_eq!(
            parse("home = \"~\"", home()).unwrap().home,
            Some(PathBuf::from("/home/u"))
        );
    }

    #[test]
    fn mistakes_are_errors_that_say_what_is_wrong() {
        let msg = |t: &str| parse(t, home()).unwrap_err().to_string();
        assert!(msg("hom = \"/x\"").contains("hom"), "unknown key");
        assert!(msg("gpu = \"fast\"").contains("swiftshader_indirect"));
        assert!(msg("home = \"relative/dir\"").contains("absolute"));
        assert!(msg("home = \"\"").contains("empty"));
        assert!(msg("min_free_mb = 0").contains("at least 1"));
        assert!(
            msg("min_free_mb = \"lots\"").contains("min_free_mb")
                || !msg("min_free_mb = \"lots\"").is_empty()
        );
        assert!(!msg("home = ").is_empty(), "not valid TOML");
        assert!(
            parse("home = \"~/x\"", None).is_err(),
            "~ without HOME is an error"
        );
    }

    #[test]
    fn the_example_file_is_valid_once_uncommented() {
        assert_eq!(parse(EXAMPLE, home()).unwrap(), Config::default());
        let live: String = EXAMPLE
            .lines()
            .map(|l| {
                l.strip_prefix("# ")
                    .filter(|r| r.contains(" = \"") || r.starts_with("min_free_mb ="))
                    .unwrap_or(l)
            })
            .collect::<Vec<_>>()
            .join("\n");
        let c = parse(&live, home()).unwrap();
        assert!(c.home.is_some() && c.sdk.is_some() && c.gpu.is_some() && c.min_free_mb.is_some());
    }
}
