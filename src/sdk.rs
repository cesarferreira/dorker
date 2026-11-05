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
            let Some(api) = name.strip_prefix("android-").and_then(parse_api) else {
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
            .filter(|i| image.flavour.as_ref().is_none_or(|f| &i.flavour == f))
            .collect();
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
        let wanted = match &image.flavour {
            Some(f) => format!("system-images;android-{};{f};{abi}", image.api),
            None => format!("system-images;android-{};google_apis;{abi}", image.api),
        };
        let have: Vec<String> = installed.iter().map(|i| i.package.clone()).collect();
        bail!(
            "no installed Android API {} system image for {abi}\n\nRun:\n  sdkmanager \"{wanted}\"\n\n\
             Installed images: {}",
            image.api,
            if have.is_empty() {
                "none".into()
            } else {
                have.join(", ")
            }
        )
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

fn parse_api(s: &str) -> Option<u32> {
    // `37.0` style directories carry a minor version; keep the major.
    s.split('.').next()?.parse().ok()
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
        assert!(err.to_string().contains("sdkmanager"));
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
