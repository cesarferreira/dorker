use crate::units::ImageRef;
use anyhow::{Result, bail};
use std::path::{Path, PathBuf};

/// A located Android SDK.
#[derive(Debug, Clone)]
pub struct Sdk {
    pub root: PathBuf,
}

/// An installed system image, discovered from the SDK directory layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemImage {
    pub api: u32,
    /// Minor SDK version for directories like `android-36.1`.
    pub minor: Option<u32>,
    pub flavour: String,
    pub abi: String,
    /// Path relative to the SDK root, with trailing slash (the `image.sysdir.1` format).
    pub sysdir: String,
    /// SDK package id, e.g. `system-images;android-35;google_apis;arm64-v8a`.
    pub package: String,
}

pub fn host_abi() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "arm64-v8a"
    } else {
        "x86_64"
    }
}

impl Sdk {
    pub fn locate() -> Result<Self> {
        let mut candidates: Vec<PathBuf> = Vec::new();
        for var in ["ANDROID_HOME", "ANDROID_SDK_ROOT"] {
            if let Some(v) = std::env::var_os(var).filter(|v| !v.is_empty()) {
                candidates.push(v.into());
            }
        }
        if let Some(home) = dirs_home() {
            candidates.push(home.join("Library/Android/sdk"));
            candidates.push(home.join("Android/Sdk"));
        }
        for c in &candidates {
            if c.join("platform-tools").is_dir() || c.join("emulator").is_dir() {
                crate::log::vlog!(
                    "android sdk: {} (checked {} candidate paths)",
                    c.display(),
                    candidates.len()
                );
                return Ok(Self { root: c.clone() });
            }
        }
        bail!(
            "Android SDK not found. Set ANDROID_HOME, or install the command-line tools:\n  \
             brew install --cask android-commandlinetools"
        )
    }

    pub fn emulator(&self) -> PathBuf {
        self.root.join("emulator").join("emulator")
    }

    pub fn adb(&self) -> PathBuf {
        self.root.join("platform-tools").join("adb")
    }

    /// A tool from `cmdline-tools/<version>/bin` (the `latest` directory first).
    fn cmdline_tool(&self, name: &str) -> Option<PathBuf> {
        let base = self.root.join("cmdline-tools");
        let mut dirs = read_dirs(&base);
        dirs.sort_by_key(|d| std::cmp::Reverse(file_name(d) == "latest"));
        dirs.into_iter()
            .map(|d| d.join("bin").join(name))
            .find(|p| p.is_file())
    }

    /// The legacy `sdkmanager`. Newer command-line tools keep it only as a deprecated shim.
    pub fn sdkmanager(&self) -> Option<PathBuf> {
        self.cmdline_tool("sdkmanager")
    }

    /// The `android` CLI (`android sdk install ...`), which replaces `sdkmanager`.
    pub fn android_cli(&self) -> Option<PathBuf> {
        self.cmdline_tool("android")
    }

    /// The stable system images Google publishes matching `pattern` (e.g. `system-images/*`),
    /// read with `android sdk list --all`. `None` when the listing cannot be read (no `android`
    /// CLI, offline, or the command failed), so callers can fall back.
    pub fn list_remote_images(&self, pattern: &str) -> Option<Vec<RemoteImage>> {
        let cli = self.android_cli()?;
        let out = std::process::Command::new(cli)
            .arg(format!("--sdk={}", self.root.display()))
            .args(["sdk", "list", "--all", pattern])
            .stdin(std::process::Stdio::null())
            .output()
            .ok()?;
        let images = parse_remote_images(&String::from_utf8_lossy(&out.stdout));
        (out.status.success() && !images.is_empty()).then_some(images)
    }

    /// The best available tool for installing SDK packages: `android` when present, else
    /// the legacy `sdkmanager` (older command-line tools), else none.
    pub fn package_installer(&self) -> Option<Installer> {
        self.android_cli()
            .map(Installer::AndroidCli)
            .or_else(|| self.sdkmanager().map(Installer::SdkManager))
    }

    /// Emulator version string, e.g. `37.2.12.0`.
    pub fn emulator_version(&self) -> Result<String> {
        let out = std::process::Command::new(self.emulator())
            .arg("-version")
            .output()?;
        let text = String::from_utf8_lossy(&out.stdout);
        text.lines()
            .find_map(|l| l.split("emulator version ").nth(1))
            .and_then(|v| v.split_whitespace().next())
            .map(str::to_string)
            .ok_or_else(|| {
                anyhow::anyhow!("could not read the emulator version from `emulator -version`")
            })
    }

    /// Revision of an installed system image, from its `package.xml`
    /// (`major.minor.micro`, trailing zero components kept).
    pub fn system_image_revision(&self, image: &SystemImage) -> Result<String> {
        let xml = std::fs::read_to_string(self.root.join(&image.sysdir).join("package.xml"))?;
        parse_revision(&xml)
            .ok_or_else(|| anyhow::anyhow!("no <revision> in package.xml for {}", image.package))
    }

    /// All installed system images, sorted by API (desc), flavour, abi.
    pub fn system_images(&self) -> Vec<SystemImage> {
        let base = self.root.join("system-images");
        let mut out = Vec::new();
        for api_dir in read_dirs(&base) {
            let name = file_name(&api_dir);
            let Some((api, minor)) = name.strip_prefix("android-").and_then(parse_api) else {
                continue;
            };
            for flavour_dir in read_dirs(&api_dir) {
                for abi_dir in read_dirs(&flavour_dir) {
                    // A usable image has a system.img (or equivalent) and a kernel.
                    if !abi_dir.join("system.img").is_file() {
                        continue;
                    }
                    let flavour = file_name(&flavour_dir);
                    let abi = file_name(&abi_dir);
                    out.push(SystemImage {
                        api,
                        minor,
                        sysdir: format!("system-images/{name}/{flavour}/{abi}/"),
                        package: format!("system-images;{name};{flavour};{abi}"),
                        flavour,
                        abi,
                    });
                }
            }
        }
        out.sort_by(|a, b| {
            b.api
                .cmp(&a.api)
                .then(a.flavour.cmp(&b.flavour))
                .then(a.abi.cmp(&b.abi))
        });
        out
    }

    /// Resolve an image reference against installed images for the host ABI.
    pub fn resolve(&self, image: &ImageRef) -> Result<SystemImage> {
        let abi = host_abi();
        let installed = self.system_images();
        let matching: Vec<&SystemImage> = installed
            .iter()
            .filter(|i| i.api == image.api && i.abi == abi)
            .filter(|i| image.minor.is_none() || i.minor == image.minor)
            .filter(|i| image.flavour.as_ref().is_none_or(|f| &i.flavour == f))
            .collect();
        // With no explicit minor, the plain major (`android-36`) beats `android-36.1`.
        let mut matching = matching;
        matching.sort_by_key(|i| i.minor.is_some());
        // Prefer the flavours most useful for automation: Google APIs without Play
        // Store first, then AOSP, then Play images.
        const PREFERENCE: [&str; 3] = ["google_apis", "default", "aosp_atd"];
        for pref in PREFERENCE {
            if let Some(i) = matching.iter().find(|i| i.flavour == pref) {
                return Ok((*i).clone());
            }
        }
        if let Some(i) = matching.first() {
            return Ok((*i).clone());
        }
        let wanted = package_for(image);
        let shown = version_label(image.api, image.minor);
        let have: Vec<String> = installed.iter().map(|i| i.package.clone()).collect();
        bail!(
            "no installed Android API {shown} system image for {abi}\n\nRun:\n  dorker pull android:{shown}\n(or: android sdk install \"{wanted}\")\n\n\
             Installed images: {}",
            if have.is_empty() {
                "none".into()
            } else {
                have.join(", ")
            }
        )
    }
}

/// A tool that can install SDK packages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Installer {
    AndroidCli(PathBuf),
    SdkManager(PathBuf),
}

impl Installer {
    pub fn path(&self) -> &Path {
        match self {
            Installer::AndroidCli(p) | Installer::SdkManager(p) => p,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Installer::AndroidCli(_) => "android sdk install",
            Installer::SdkManager(_) => "sdkmanager",
        }
    }

    /// The command that installs `package` into the SDK at `root`.
    pub fn command(&self, root: &Path, package: &str) -> std::process::Command {
        let mut c = std::process::Command::new(self.path());
        match self {
            Installer::AndroidCli(_) => {
                c.arg(format!("--sdk={}", root.display()))
                    .args(["sdk", "install", package]);
            }
            Installer::SdkManager(_) => {
                c.arg(format!("--sdk_root={}", root.display()))
                    .args(["--install", package]);
            }
        }
        c
    }
}

/// The SDK package id for an image reference on this host.
pub fn package_for(image: &ImageRef) -> String {
    format!(
        "system-images;android-{};{};{}",
        version_label(image.api, image.minor),
        image.flavour.as_deref().unwrap_or("google_apis"),
        host_abi()
    )
}

/// A system image published in the SDK repository (a line of `android sdk list --all`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteImage {
    /// Package path, e.g. `system-images/android-37.0/google_apis/x86_64`.
    pub path: String,
    pub api: u32,
    pub minor: Option<u32>,
    pub flavour: String,
    pub abi: String,
}

impl RemoteImage {
    /// The legacy `;`-separated package id, the form `SystemImage::package` uses.
    pub fn package_id(&self) -> String {
        self.path.replace('/', ";")
    }

    /// The image reference that selects exactly this image, e.g. `android:37.0-google_apis`.
    pub fn reference(&self) -> String {
        format!(
            "android:{}-{}",
            version_label(self.api, self.minor),
            self.flavour
        )
    }
}

/// Stable system images in `android sdk list --all` output. Canary, beta and other
/// preview directories (`android-CANARY`, `android-37.2-beta1`, ...) are skipped.
pub fn parse_remote_images(list_output: &str) -> Vec<RemoteImage> {
    let mut out = Vec::new();
    for line in list_output.lines() {
        let Some(path) = line
            .split_whitespace()
            .next()
            .filter(|p| p.starts_with("system-images/"))
        else {
            continue;
        };
        let parts: Vec<&str> = path.split('/').collect();
        let [_, version_dir, flavour, abi] = parts.as_slice() else {
            continue;
        };
        let Some((api, minor)) = version_dir.strip_prefix("android-").and_then(parse_api) else {
            continue; // android-CANARY, android-37.2-beta1, android-canary-2026...
        };
        out.push(RemoteImage {
            path: path.to_string(),
            api,
            minor,
            flavour: flavour.to_string(),
            abi: abi.to_string(),
        });
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out.dedup();
    out
}

/// Preference among phone flavours when none is requested; lower is better. `None` means the
/// flavour is not a default choice (Wear, TV, tablet, automotive...): only chosen explicitly.
pub fn flavour_rank(flavour: &str) -> Option<u8> {
    Some(match flavour {
        "google_apis" => 0,
        "default" => 1,
        "aosp_atd" => 2,
        "google_atd" => 3,
        "google_apis_ps16k" => 4,
        "google_apis_playstore" => 5,
        "google_apis_playstore_ps16k" => 6,
        _ => return None,
    })
}

/// Pick the published image an image reference means, for `abi`. Without an explicit minor the
/// base release wins (`android-37`, then `37.0`, then the newest minor); without an explicit
/// flavour the best phone flavour wins. `Err` carries every image published for that API and
/// ABI, so the caller can show the user what exists.
pub fn choose_remote(
    images: &[RemoteImage],
    r: &ImageRef,
    abi: &str,
) -> Result<RemoteImage, Vec<RemoteImage>> {
    let for_api: Vec<&RemoteImage> = images
        .iter()
        .filter(|i| i.api == r.api && i.abi == abi && r.minor.is_none_or(|m| i.minor == Some(m)))
        .collect();
    let minor_pref = |m: Option<u32>| match m {
        None => (0, 0),
        Some(0) => (1, 0),
        Some(m) => (2, u32::MAX - m), // newest minor first
    };
    let best = for_api
        .iter()
        .filter_map(|i| match &r.flavour {
            Some(f) if &i.flavour == f => Some((minor_pref(i.minor), 0, *i)),
            Some(_) => None,
            None => flavour_rank(&i.flavour).map(|rank| (minor_pref(i.minor), rank, *i)),
        })
        .min_by_key(|(m, rank, _)| (*m, *rank));
    match best {
        Some((_, _, img)) => Ok(img.clone()),
        None => Err(for_api.into_iter().cloned().collect()),
    }
}

/// `36` or `36.1`.
pub fn version_label(api: u32, minor: Option<u32>) -> String {
    match minor {
        Some(m) => format!("{api}.{m}"),
        None => api.to_string(),
    }
}

impl Sdk {
    /// An installed image by exact SDK package id (`system-images;android-36.1;...`).
    pub fn find_package(&self, package: &str) -> Option<SystemImage> {
        self.system_images()
            .into_iter()
            .find(|i| i.package == package)
    }
}

/// Extract the first `<revision>` block's major/minor/micro as `a.b.c`.
pub fn parse_revision(xml: &str) -> Option<String> {
    let start = xml.find("<revision>")?;
    let block = &xml[start..start + xml[start..].find("</revision>")?];
    let part = |tag: &str| -> Option<String> {
        let open = format!("<{tag}>");
        let from = block.find(&open)? + open.len();
        let len = block[from..].find(&format!("</{tag}>"))?;
        Some(block[from..from + len].trim().to_string())
    };
    let major = part("major")?;
    Some(format!(
        "{major}.{}.{}",
        part("minor").unwrap_or_else(|| "0".into()),
        part("micro").unwrap_or_else(|| "0".into())
    ))
}

fn parse_api(s: &str) -> Option<(u32, Option<u32>)> {
    // `37.0` style directories carry a minor version.
    match s.split_once('.') {
        Some((a, b)) => Some((a.parse().ok()?, Some(b.parse().ok()?))),
        None => Some((s.parse().ok()?, None)),
    }
}

fn read_dirs(p: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(p)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    v.sort();
    v
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

pub fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_sdk(layout: &[(&str, &str, &str)]) -> (crate::testutil::Dir, Sdk) {
        let dir = crate::testutil::Dir::new();
        for (api, flavour, abi) in layout {
            let d = dir
                .path()
                .join(format!("system-images/android-{api}/{flavour}/{abi}"));
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("system.img"), b"x").unwrap();
        }
        let sdk = Sdk {
            root: dir.path().to_path_buf(),
        };
        (dir, sdk)
    }

    #[test]
    fn resolves_preferred_flavour() {
        let abi = host_abi();
        let (_d, sdk) = fake_sdk(&[
            ("35", "google_apis_playstore", abi),
            ("35", "google_apis", abi),
            ("36", "google_apis", abi),
        ]);
        let r = sdk
            .resolve(&ImageRef::parse("android:35").unwrap())
            .unwrap();
        assert_eq!(r.flavour, "google_apis");
        assert_eq!(
            r.package,
            format!("system-images;android-35;google_apis;{abi}")
        );
        assert_eq!(
            r.sysdir,
            format!("system-images/android-35/google_apis/{abi}/")
        );
    }

    #[test]
    fn minor_versions_are_distinct() {
        let abi = host_abi();
        let d = crate::testutil::Dir::new();
        for api in ["36", "36.1"] {
            let p = d
                .path()
                .join(format!("system-images/android-{api}/google_apis/{abi}"));
            std::fs::create_dir_all(&p).unwrap();
            std::fs::write(p.join("system.img"), b"x").unwrap();
        }
        let sdk = Sdk {
            root: d.path().to_path_buf(),
        };
        assert_eq!(sdk.system_images().len(), 2);
        // Plain `android:36` prefers the plain major directory.
        let r = sdk
            .resolve(&ImageRef::parse("android:36").unwrap())
            .unwrap();
        assert_eq!(r.minor, None);
        // An explicit minor selects exactly that one.
        let r = sdk
            .resolve(&ImageRef::parse("android:36.1").unwrap())
            .unwrap();
        assert_eq!(r.minor, Some(1));
        assert!(r.package.contains("android-36.1"));
        assert!(
            sdk.resolve(&ImageRef::parse("android:36.2").unwrap())
                .is_err()
        );
        assert!(sdk.find_package(&r.package).is_some());
        assert_eq!(
            package_for(&ImageRef::parse("android:36.1").unwrap()),
            format!("system-images;android-36.1;google_apis;{abi}")
        );
    }

    #[test]
    fn explicit_flavour_and_missing() {
        let abi = host_abi();
        let (_d, sdk) = fake_sdk(&[("35", "google_apis_playstore", abi)]);
        let r = sdk
            .resolve(&ImageRef::parse("android:35-google_apis_playstore").unwrap())
            .unwrap();
        assert_eq!(r.flavour, "google_apis_playstore");
        let err = sdk
            .resolve(&ImageRef::parse("android:34").unwrap())
            .unwrap_err();
        assert!(err.to_string().contains("dorker pull android:34"));
        assert!(err.to_string().contains("android sdk install"));
    }

    /// Lines copied from real `android sdk list --all "system-images/android-37*"` output.
    const LIST_37: &str = "Installed packages:
  system-images/android-37.0/google_apis_playstore_ps16k/arm64-v8a               7.0.0   ->        8.0.0  16 KB Page Size Google Play ARM 64 v8a System Image
Available packages:
  system-images/android-37.0/android-wear-signed/x86_64                          1.0.0                    Wear OS 7.0 Intel x86_64 Atom System Image (signed)
  system-images/android-37.0/aosp_atd/x86_64                                     1.0.0                    AOSP ATD Intel x86_64 Atom System Image
  system-images/android-37.0/default/x86_64                                      6.0.0                    Intel x86_64 Atom System Image
  system-images/android-37.0/google_apis/arm64-v8a                               8.0.0                    Google APIs ARM 64 v8a System Image
  system-images/android-37.0/google_apis/x86_64                                  8.0.0                    Google APIs Intel x86_64 Atom System Image
  system-images/android-37.0/google_apis_playstore/x86_64                        8.0.0                    Google Play Intel x86_64 Atom System Image
  system-images/android-37.0/google_apis_ps16k/x86_64                            8.0.0                    16 KB Page Size Google APIs Intel x86_64 Atom System Image
  system-images/android-37.0/google_atd/x86_64                                   7.0.0                    Google APIs ATD Intel x86_64 Atom System Image
  system-images/android-37.1/google_apis_playstore_ps16k/x86_64                  9.0.0                    16 KB Page Size Google Play Intel x86_64 Atom System Image
  system-images/android-37.1/google_apis_ps16k/x86_64                            9.0.0                    16 KB Page Size Google APIs Intel x86_64 Atom System Image
  system-images/android-37.2-beta1/google_apis_ps16k/x86_64                      1.0.0                    16 KB Page Size Google APIs Intel x86_64 Atom System Image
  system-images/android-CANARY/google_apis_ps16k/x86_64                          15.0.0                   16 KB Page Size Google APIs Intel x86_64 Atom System Image
  system-images/android-canary-20261104/google_apis_ps16k/x86_64                 18.0.0                   16 KB Page Size Google APIs Intel x86_64 Atom System Image
";

    #[test]
    fn parses_the_real_repository_listing_and_skips_previews() {
        let imgs = parse_remote_images(LIST_37);
        assert_eq!(imgs.len(), 11, "{imgs:#?}");
        assert!(imgs.iter().all(|i| i.api == 37));
        assert!(
            imgs.iter()
                .any(|i| i.path == "system-images/android-37.0/google_apis/x86_64")
        );
        assert!(!imgs.iter().any(|i| i.path.contains("beta")
            || i.path.contains("CANARY")
            || i.path.contains("canary")));
        let one = imgs
            .iter()
            .find(|i| i.path.ends_with("android-37.1/google_apis_ps16k/x86_64"))
            .unwrap();
        assert_eq!(
            (one.minor, one.flavour.as_str(), one.abi.as_str()),
            (Some(1), "google_apis_ps16k", "x86_64")
        );
        assert_eq!(
            one.package_id(),
            "system-images;android-37.1;google_apis_ps16k;x86_64"
        );
        assert_eq!(one.reference(), "android:37.1-google_apis_ps16k");
        assert!(parse_remote_images("garbage\nplatforms/android-34 1 x").is_empty());
    }

    #[test]
    fn android_37_means_the_base_release_with_the_best_phone_flavour() {
        // The bug: dorker asked for `android-37`, which is not published; it is `android-37.0`.
        let imgs = parse_remote_images(LIST_37);
        let pick = choose_remote(&imgs, &ImageRef::parse("android:37").unwrap(), "x86_64").unwrap();
        assert_eq!(pick.path, "system-images/android-37.0/google_apis/x86_64");
        // arm64 has its own images.
        let arm =
            choose_remote(&imgs, &ImageRef::parse("android:37").unwrap(), "arm64-v8a").unwrap();
        assert_eq!(arm.path, "system-images/android-37.0/google_apis/arm64-v8a");
    }

    #[test]
    fn explicit_minor_and_flavour_are_honoured() {
        let imgs = parse_remote_images(LIST_37);
        let pick = |r: &str| choose_remote(&imgs, &ImageRef::parse(r).unwrap(), "x86_64");
        // 37.1 only publishes 16 KB flavours: the best of those is chosen.
        assert_eq!(
            pick("android:37.1").unwrap().path,
            "system-images/android-37.1/google_apis_ps16k/x86_64"
        );
        assert_eq!(
            pick("android:37-google_apis_playstore").unwrap().path,
            "system-images/android-37.0/google_apis_playstore/x86_64"
        );
        assert_eq!(
            pick("android:37.1-google_apis_playstore_ps16k")
                .unwrap()
                .minor,
            Some(1)
        );
        // Non-phone flavours are never a default, only an explicit choice.
        assert_eq!(pick("android:37-google_atd").unwrap().flavour, "google_atd");
        assert!(pick("android:37-android-wear-signed").is_ok());
    }

    #[test]
    fn unavailable_requests_return_what_does_exist() {
        let imgs = parse_remote_images(LIST_37);
        let err = choose_remote(
            &imgs,
            &ImageRef::parse("android:37-nonexistent").unwrap(),
            "x86_64",
        )
        .unwrap_err();
        assert!(
            err.iter().any(|i| i.flavour == "google_apis") && err.len() >= 8,
            "{err:?}"
        );
        assert!(
            choose_remote(&imgs, &ImageRef::parse("android:99").unwrap(), "x86_64")
                .unwrap_err()
                .is_empty()
        );
        assert!(
            choose_remote(&imgs, &ImageRef::parse("android:37.9").unwrap(), "x86_64")
                .unwrap_err()
                .is_empty()
        );
        // Only non-default flavours published for an API: nothing is picked automatically.
        let only_wear: Vec<RemoteImage> = imgs
            .iter()
            .filter(|i| i.flavour == "android-wear-signed")
            .cloned()
            .collect();
        assert!(
            choose_remote(
                &only_wear,
                &ImageRef::parse("android:37").unwrap(),
                "x86_64"
            )
            .is_err()
        );
    }

    #[test]
    fn plain_major_beats_a_minor_release_and_newest_minor_wins_otherwise() {
        let mk = |p: &str| parse_remote_images(&format!("  {p}  1.0.0  x"));
        let mut imgs = mk("system-images/android-36/google_apis/x86_64");
        imgs.extend(mk("system-images/android-36.1/google_apis/x86_64"));
        let r = ImageRef::parse("android:36").unwrap();
        assert_eq!(
            choose_remote(&imgs, &r, "x86_64").unwrap().minor,
            None,
            "plain android-36 first"
        );
        let mut only_minors = mk("system-images/android-40.0/google_apis/x86_64");
        only_minors.extend(mk("system-images/android-40.2/google_apis/x86_64"));
        only_minors.extend(mk("system-images/android-40.1/google_apis/x86_64"));
        let r = ImageRef::parse("android:40").unwrap();
        assert_eq!(
            choose_remote(&only_minors, &r, "x86_64").unwrap().minor,
            Some(0),
            "base release .0 first"
        );
        let no_zero: Vec<RemoteImage> = only_minors
            .into_iter()
            .filter(|i| i.minor != Some(0))
            .collect();
        assert_eq!(
            choose_remote(&no_zero, &r, "x86_64").unwrap().minor,
            Some(2),
            "then the newest minor"
        );
    }

    #[test]
    fn installer_prefers_the_android_cli_over_the_deprecated_sdkmanager() {
        let d = crate::testutil::Dir::new();
        let sdk = Sdk {
            root: d.path().to_path_buf(),
        };
        assert_eq!(sdk.package_installer(), None);
        let bin = d.path().join("cmdline-tools/latest/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("sdkmanager"), b"#!/bin/sh\n").unwrap();
        let only_legacy = sdk.package_installer().unwrap();
        assert!(matches!(only_legacy, Installer::SdkManager(_)));
        assert_eq!(only_legacy.name(), "sdkmanager");
        std::fs::write(bin.join("android"), b"#!/bin/sh\n").unwrap();
        let both = sdk.package_installer().unwrap();
        assert!(
            matches!(both, Installer::AndroidCli(_)),
            "the new CLI wins when both exist"
        );
        let args = |i: &Installer| -> Vec<String> {
            i.command(
                Path::new("/sdk"),
                "system-images;android-35;google_apis;x86_64",
            )
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
        };
        assert_eq!(
            args(&both),
            [
                "--sdk=/sdk",
                "sdk",
                "install",
                "system-images;android-35;google_apis;x86_64"
            ]
        );
        assert_eq!(
            args(&only_legacy),
            [
                "--sdk_root=/sdk",
                "--install",
                "system-images;android-35;google_apis;x86_64"
            ]
        );
    }

    #[test]
    fn package_names() {
        let abi = host_abi();
        assert_eq!(
            package_for(&ImageRef::parse("android:35").unwrap()),
            format!("system-images;android-35;google_apis;{abi}")
        );
        assert_eq!(
            package_for(&ImageRef::parse("android:34-google_apis_playstore").unwrap()),
            format!("system-images;android-34;google_apis_playstore;{abi}")
        );
    }

    #[test]
    fn revision_parsing() {
        let xml = "<a><revision>\n<major>9</major>\n<minor>2</minor>\n</revision><x><revision><major>1</major></revision></x></a>";
        assert_eq!(parse_revision(xml).as_deref(), Some("9.2.0"));
        assert_eq!(parse_revision("<a/>"), None);
    }

    #[test]
    fn ignores_wrong_abi() {
        let other = if host_abi() == "arm64-v8a" {
            "x86_64"
        } else {
            "arm64-v8a"
        };
        let (_d, sdk) = fake_sdk(&[("35", "google_apis", other)]);
        assert!(
            sdk.resolve(&ImageRef::parse("android:35").unwrap())
                .is_err()
        );
    }
}
