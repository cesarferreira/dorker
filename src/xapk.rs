//! Install an app from a file: a plain `.apk`, or an `.xapk` bundle.
//!
//! An XAPK is a zip with the app's split APKs (`base` plus config splits, which apps such as
//! Android Auto ship as) and an optional `manifest.json` naming them, plus optional OBB
//! expansion files. Android installs the splits together with `adb install-multiple`, and the
//! OBBs are pushed to shared storage where the app looks for them.

use crate::sdk::Sdk;
use anyhow::{Context, Result, bail};
use std::io::Read;
use std::path::{Path, PathBuf};

/// Refuse to unpack more than this (zip bomb guard; real bundles with OBBs are a few GB).
const MAX_UNPACKED_BYTES: u64 = 8 << 30;
/// `manifest.json` is read into memory, so it gets a small fixed limit.
const MAX_MANIFEST_BYTES: u64 = 1 << 20;

/// True when `path` should be unpacked as an XAPK bundle rather than installed as one APK.
pub fn is_bundle(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("xapk"))
}

/// What to install from a bundle: zip entry names, base APK first.
#[derive(Debug, PartialEq, Eq)]
pub struct Plan {
    pub apks: Vec<String>,
    /// (zip entry name, absolute path on the device)
    pub obbs: Vec<(String, String)>,
}

fn file_name(entry: &str) -> &str {
    entry.rsplit('/').next().unwrap_or(entry)
}

/// A relative device path made only of plain components (letters, digits, `.`, `_`, `-`).
/// The path ends up in a device shell command, so anything else (spaces, quotes, `;`, `$`,
/// backticks) is refused, as are empty and `..` components.
fn safe_device_path(p: &str) -> bool {
    !p.is_empty()
        && p.split('/').all(|c| {
            !c.is_empty()
                && c != ".."
                && c.chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
        })
}

/// Decide what to install from the zip's entry names and its `manifest.json` (if any).
/// Pure, so it is unit-tested.
pub fn plan(manifest: Option<&serde_json::Value>, entries: &[String]) -> Result<Plan> {
    let package = manifest
        .and_then(|m| m["package_name"].as_str())
        .map(str::to_string);
    let have = |name: &str| entries.iter().any(|e| e == name);

    let mut apks: Vec<String> = Vec::new();
    let mut base: Option<String> = None;
    if let Some(splits) = manifest.and_then(|m| m["split_apks"].as_array()) {
        for s in splits {
            let Some(file) = s["file"].as_str() else {
                continue;
            };
            if !have(file) {
                bail!("manifest.json lists {file}, which is not in the bundle");
            }
            if s["id"].as_str() == Some("base") {
                base = Some(file.to_string());
            }
            apks.push(file.to_string());
        }
    }
    if apks.is_empty() {
        // No (usable) manifest: every APK in the bundle.
        apks = entries
            .iter()
            .filter(|e| e.to_ascii_lowercase().ends_with(".apk"))
            .cloned()
            .collect();
    }
    if apks.is_empty() {
        bail!("the bundle contains no .apk files");
    }
    // The base APK must come first: `install-multiple` treats the first as the base.
    if base.is_none() {
        base = package
            .as_ref()
            .and_then(|p| apks.iter().find(|a| file_name(a) == format!("{p}.apk")))
            .cloned();
    }
    if let Some(b) = base
        && let Some(i) = apks.iter().position(|a| *a == b)
    {
        let b = apks.remove(i);
        apks.insert(0, b);
    }

    let mut obbs = Vec::new();
    if let Some(list) = manifest.and_then(|m| m["expansions"].as_array()) {
        for x in list {
            let Some(file) = x["file"].as_str() else {
                continue;
            };
            if !have(file) {
                bail!("manifest.json lists {file}, which is not in the bundle");
            }
            let install_path = match x["install_path"].as_str() {
                Some(p) => p.trim_start_matches('/').to_string(),
                None => match &package {
                    Some(p) => format!("Android/obb/{p}/{}", file_name(file)),
                    None => {
                        bail!("{file} has no install_path and the manifest has no package_name")
                    }
                },
            };
            if !safe_device_path(&install_path) {
                bail!("unsafe install_path {install_path:?} in manifest.json");
            }
            obbs.push((file.to_string(), format!("/sdcard/{install_path}")));
        }
    }
    Ok(Plan { apks, obbs })
}

/// A bundle unpacked into a private temporary directory, removed when dropped.
pub struct Staged {
    dir: PathBuf,
    pub apks: Vec<PathBuf>,
    pub obbs: Vec<(PathBuf, String)>,
}

impl Drop for Staged {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn private_dir() -> Result<PathBuf> {
    // create_dir (not create_dir_all) fails if the name exists, so an attacker cannot
    // pre-create it in a shared temp directory.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("dorker-xapk-{}-{nanos}", std::process::id()));
    std::fs::create_dir(&dir).with_context(|| format!("creating {}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(dir)
}

/// Unpack the files of a bundle that need installing.
pub fn stage(bundle: &Path) -> Result<Staged> {
    let file =
        std::fs::File::open(bundle).with_context(|| format!("opening {}", bundle.display()))?;
    let mut zip = zip::ZipArchive::new(file)
        .with_context(|| format!("{} is not a valid .xapk (zip) file", bundle.display()))?;
    let names: Vec<String> = (0..zip.len())
        .filter_map(|i| zip.by_index(i).ok().map(|e| e.name().to_string()))
        .collect();
    let manifest = match zip.by_name("manifest.json") {
        Ok(mut m) => {
            let mut text = String::new();
            // A real manifest is a few KB; do not trust the declared size.
            (&mut m)
                .take(MAX_MANIFEST_BYTES + 1)
                .read_to_string(&mut text)
                .context("reading manifest.json")?;
            if text.len() as u64 > MAX_MANIFEST_BYTES {
                bail!(
                    "manifest.json is larger than {} MB",
                    MAX_MANIFEST_BYTES >> 20
                );
            }
            Some(
                serde_json::from_str::<serde_json::Value>(&text)
                    .context("manifest.json is not valid JSON")?,
            )
        }
        Err(_) => None,
    };
    let plan = plan(manifest.as_ref(), &names)?;

    let dir = private_dir()?;
    let mut staged = Staged {
        dir: dir.clone(),
        apks: Vec::new(),
        obbs: Vec::new(),
    };
    let mut total = 0u64;
    let mut extract = |name: &str| -> Result<PathBuf> {
        let mut entry = zip
            .by_name(name)
            .with_context(|| format!("reading {name}"))?;
        // `enclosed_name` rejects absolute paths and `..` components.
        let rel = entry
            .enclosed_name()
            .with_context(|| format!("unsafe path {name:?} in the bundle"))?;
        // The size in the zip header is only a hint, so also count the bytes really written
        // and stop one byte past the remaining budget.
        let budget = MAX_UNPACKED_BYTES.saturating_sub(total);
        if entry.size() > budget {
            bail!(
                "the bundle unpacks to more than {} GB",
                MAX_UNPACKED_BYTES >> 30
            );
        }
        let out = dir.join(rel);
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut f = std::fs::File::create(&out)?;
        let written = std::io::copy(&mut (&mut entry).take(budget + 1), &mut f)
            .with_context(|| format!("unpacking {name}"))?;
        total += written;
        if total > MAX_UNPACKED_BYTES {
            bail!(
                "the bundle unpacks to more than {} GB",
                MAX_UNPACKED_BYTES >> 30
            );
        }
        Ok(out)
    };
    for a in &plan.apks {
        let p = extract(a)?;
        staged.apks.push(p);
    }
    for (name, device) in &plan.obbs {
        let p = extract(name)?;
        staged.obbs.push((p, device.clone()));
    }
    Ok(staged)
}

/// Install `path` (an `.apk` or an `.xapk`) on the device. Returns adb's output, and fails
/// with it when adb reports a failure (adb can exit 0 with `Failure [...]`).
pub fn install(sdk: &Sdk, serial: &str, path: &Path, what: &str) -> Result<String> {
    if !is_bundle(path) {
        return crate::build::adb_checked(
            sdk,
            serial,
            &["install", "-r", &path.to_string_lossy()],
            &format!("installing {what}"),
        );
    }
    let staged = stage(path)?;
    let apks: Vec<String> = staged
        .apks
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    let mut args = vec!["install-multiple", "-r"];
    args.extend(apks.iter().map(String::as_str));
    let mut output = crate::build::adb_checked(
        sdk,
        serial,
        &args,
        &format!("installing {what} ({} APKs)", apks.len()),
    )?;
    for (local, device) in &staged.obbs {
        let dir = device.rsplit_once('/').map_or("/sdcard", |(d, _)| d);
        crate::build::adb_checked(
            sdk,
            serial,
            // `adb shell` joins its arguments into one shell line. The path was
            // checked by `safe_device_path` in `plan`, and is single-quoted as well.
            &["shell", &format!("mkdir -p '{dir}'")],
            "creating the OBB folder",
        )?;
        output.push_str(&crate::build::adb_checked(
            sdk,
            serial,
            &["push", &local.to_string_lossy(), device],
            &format!(
                "copying {} to {device}",
                file_name(&local.to_string_lossy())
            ),
        )?);
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn names(n: &[&str]) -> Vec<String> {
        n.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn recognises_the_extension_case_insensitively() {
        assert!(is_bundle(Path::new("a/b.xapk")));
        assert!(is_bundle(Path::new("B.XAPK")));
        assert!(!is_bundle(Path::new("b.apk")));
        assert!(!is_bundle(Path::new("xapk")));
    }

    #[test]
    fn manifest_splits_install_base_first() {
        let m = json!({
            "package_name": "com.x",
            "split_apks": [
                {"file": "config.arm64_v8a.apk", "id": "config.arm64_v8a"},
                {"file": "com.x.apk", "id": "base"},
                {"file": "config.xxhdpi.apk", "id": "config.xxhdpi"},
            ],
        });
        let p = plan(
            Some(&m),
            &names(&[
                "config.arm64_v8a.apk",
                "com.x.apk",
                "config.xxhdpi.apk",
                "manifest.json",
            ]),
        )
        .unwrap();
        assert_eq!(p.apks[0], "com.x.apk");
        assert_eq!(p.apks.len(), 3);
        assert!(p.obbs.is_empty());
    }

    #[test]
    fn base_is_found_by_package_name_when_ids_are_missing() {
        let m = json!({"package_name": "com.x", "split_apks": [{"file": "a.apk"}, {"file": "com.x.apk"}]});
        let p = plan(Some(&m), &names(&["a.apk", "com.x.apk"])).unwrap();
        assert_eq!(p.apks, vec!["com.x.apk", "a.apk"]);
    }

    #[test]
    fn without_a_manifest_every_apk_is_installed() {
        let p = plan(None, &names(&["x/one.apk", "two.APK", "readme.txt"])).unwrap();
        assert_eq!(p.apks, vec!["x/one.apk", "two.APK"]);
    }

    #[test]
    fn bundles_without_apks_or_with_missing_files_are_rejected() {
        assert!(plan(None, &names(&["readme.txt"])).is_err());
        let m = json!({"split_apks": [{"file": "gone.apk", "id": "base"}]});
        let err = plan(Some(&m), &names(&["other.apk"]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("gone.apk"), "{err}");
    }

    #[test]
    fn obb_files_go_to_shared_storage_and_unsafe_paths_are_rejected() {
        let m = json!({
            "package_name": "com.x",
            "split_apks": [{"file": "com.x.apk", "id": "base"}],
            "expansions": [
                {"file": "main.1.com.x.obb", "install_path": "Android/obb/com.x/main.1.com.x.obb"},
                {"file": "patch.obb"},
            ],
        });
        let p = plan(
            Some(&m),
            &names(&["com.x.apk", "main.1.com.x.obb", "patch.obb"]),
        )
        .unwrap();
        assert_eq!(
            p.obbs,
            vec![
                (
                    "main.1.com.x.obb".into(),
                    "/sdcard/Android/obb/com.x/main.1.com.x.obb".into()
                ),
                (
                    "patch.obb".into(),
                    "/sdcard/Android/obb/com.x/patch.obb".into()
                ),
            ]
        );
        for evil in [
            "../../etc/o.obb",
            "Android/obb/x;reboot;/o.obb",
            "Android/obb/$(id)/o.obb",
            "Android/obb/a b/o.obb",
            "Android/obb/`id`/o.obb",
            "Android/obb/a'b/o.obb",
        ] {
            let bad = json!({
                "package_name": "com.x",
                "split_apks": [{"file": "com.x.apk"}],
                "expansions": [{"file": "o.obb", "install_path": evil}],
            });
            let err = plan(Some(&bad), &names(&["com.x.apk", "o.obb"]))
                .unwrap_err()
                .to_string();
            assert!(err.contains("unsafe"), "{evil}: {err}");
        }
    }

    fn write_zip(path: &Path, files: &[(&str, &[u8])]) {
        use std::io::Write;
        let f = std::fs::File::create(path).unwrap();
        let mut z = zip::ZipWriter::new(f);
        for (name, data) in files {
            z.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            z.write_all(data).unwrap();
        }
        z.finish().unwrap();
    }

    #[test]
    fn stage_unpacks_apks_and_obbs_and_cleans_up() {
        let tmp = std::env::temp_dir().join(format!("dorker-xapk-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let bundle = tmp.join("app.xapk");
        let manifest = json!({
            "package_name": "com.x",
            "split_apks": [{"file": "config.apk", "id": "config"}, {"file": "com.x.apk", "id": "base"}],
            "expansions": [{"file": "main.obb", "install_path": "Android/obb/com.x/main.obb"}],
        })
        .to_string();
        write_zip(
            &bundle,
            &[
                ("manifest.json", manifest.as_bytes()),
                ("config.apk", b"cfg"),
                ("com.x.apk", b"base"),
                ("main.obb", b"obb"),
            ],
        );
        let dir;
        {
            let s = stage(&bundle).unwrap();
            dir = s.dir.clone();
            assert_eq!(s.apks.len(), 2);
            assert_eq!(std::fs::read(&s.apks[0]).unwrap(), b"base");
            assert_eq!(std::fs::read(&s.apks[1]).unwrap(), b"cfg");
            assert_eq!(s.obbs[0].1, "/sdcard/Android/obb/com.x/main.obb");
            assert_eq!(std::fs::read(&s.obbs[0].0).unwrap(), b"obb");
            assert!(dir.exists());
        }
        assert!(!dir.exists(), "staging directory is removed on drop");
        // Not a zip at all: a clear error.
        let junk = tmp.join("junk.xapk");
        std::fs::write(&junk, b"not a zip").unwrap();
        let err = stage(&junk).err().unwrap().to_string();
        assert!(err.contains("not a valid .xapk"), "{err}");
        // An oversized manifest is refused rather than read into memory.
        let big = tmp.join("big.xapk");
        let filler = vec![b' '; (MAX_MANIFEST_BYTES as usize) + 10];
        write_zip(&big, &[("manifest.json", &filler), ("a.apk", b"x")]);
        let err = stage(&big).err().unwrap().to_string();
        assert!(err.contains("manifest.json is larger"), "{err}");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
