//! Immutable logical images: a prepared AVD home (+ optional snapshot) with metadata.
//!
//! ```text
//! <root>/images/<id>/image.json   metadata (tags are the only mutable field)
//! <root>/images/<id>/avd/         golden AVD home; instances clone it, never run it
//! <root>/images/<id>/build.log    emulator log from the build
//! ```

use crate::store::Store;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageMeta {
    pub id: String,
    pub fingerprint: String,
    pub tags: Vec<String>,
    pub api: u32,
    pub flavour: String,
    pub abi: String,
    pub system_image: String,
    pub system_image_revision: String,
    pub emulator_version: String,
    pub memory_mb: u64,
    pub cpus: u32,
    pub disk_mb: u64,
    pub device: String,
    pub width: u32,
    pub height: u32,
    pub headless: bool,
    #[serde(default = "default_gpu")]
    pub gpu: String,
    pub snapshot: Option<String>,
    /// Package launched after startup (`CMD launch <package>`).
    pub launch: Option<String>,
    pub created_at: u64,
}

fn default_gpu() -> String {
    "swiftshader_indirect".into()
}

pub fn image_dir(store: &Store, id: &str) -> PathBuf {
    store.images_dir().join(id)
}

pub fn avd_home(store: &Store, id: &str) -> PathBuf {
    image_dir(store, id).join("avd")
}

pub fn save_meta(store: &Store, meta: &ImageMeta) -> Result<()> {
    let dir = image_dir(store, &meta.id);
    let tmp = dir.join("image.json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(meta)?)?;
    fs::rename(&tmp, dir.join("image.json"))?;
    Ok(())
}

pub fn list(store: &Store) -> Result<Vec<ImageMeta>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(store.images_dir())? {
        let path = entry?.path().join("image.json");
        let Ok(bytes) = fs::read(&path) else { continue };
        if let Ok(m) = serde_json::from_slice::<ImageMeta>(&bytes) {
            out.push(m);
        }
    }
    out.sort_by_key(|m| std::cmp::Reverse(m.created_at));
    Ok(out)
}

/// Normalise `repo` to `repo:latest` and validate the syntax.
pub fn normalize_tag(input: &str) -> Result<String> {
    let (repo, tag) = input.split_once(':').unwrap_or((input, "latest"));
    let repo_ok = !repo.is_empty()
        && repo.chars().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-' | '/')
        });
    let tag_ok = !tag.is_empty()
        && tag
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if !repo_ok || !tag_ok {
        bail!("invalid image name {input:?}: use lowercase `repository[:tag]`");
    }
    Ok(format!("{repo}:{tag}"))
}

/// Find by `repo[:tag]` or by (prefix of) image id.
pub fn find(store: &Store, key: &str) -> Result<ImageMeta> {
    let all = list(store)?;
    if let Ok(tag) = normalize_tag(key) {
        if let Some(m) = all.iter().find(|m| m.tags.contains(&tag)) {
            return Ok(m.clone());
        }
    }
    let by_id: Vec<&ImageMeta> = all.iter().filter(|m| m.id.starts_with(key)).collect();
    match by_id.as_slice() {
        [one] => Ok((*one).clone()),
        [] => bail!(
            "no such image: {key}\n\nRun `dorker images` to list images, or `dorker build` to create one."
        ),
        _ => bail!("ambiguous image id prefix {key:?}"),
    }
}

/// Point `tag` at `meta`, removing it from any other image (like Docker).
pub fn apply_tag(store: &Store, meta: &mut ImageMeta, tag: &str) -> Result<()> {
    for mut other in list(store)? {
        if other.id != meta.id && other.tags.iter().any(|t| t == tag) {
            other.tags.retain(|t| t != tag);
            save_meta(store, &other)?;
        }
    }
    if !meta.tags.iter().any(|t| t == tag) {
        meta.tags.push(tag.to_string());
    }
    save_meta(store, meta)
}

pub fn remove(store: &Store, meta: &ImageMeta) -> Result<()> {
    let dir = image_dir(store, &meta.id);
    if dir.parent() != Some(store.images_dir().as_path())
        || meta.id.is_empty()
        || meta.id.contains('/')
    {
        bail!("refusing to remove {}", dir.display());
    }
    fs::remove_dir_all(&dir).with_context(|| format!("removing {}", dir.display()))
}

/// (logical bytes, bytes actually allocated on disk) of a directory tree.
/// Allocated size is lower for sparse files; clones are counted in full.
pub fn dir_usage(path: &Path) -> (u64, u64) {
    let mut logical = 0;
    let mut allocated = 0;
    let mut stack = vec![path.to_path_buf()];
    while let Some(p) = stack.pop() {
        let Ok(rd) = fs::read_dir(&p) else { continue };
        for e in rd.flatten() {
            let Ok(md) = e.path().symlink_metadata() else {
                continue;
            };
            if md.is_dir() {
                stack.push(e.path());
            } else {
                logical += md.len();
                allocated += md.blocks() * 512;
            }
        }
    }
    (logical, allocated)
}

pub fn human(bytes: u64) -> String {
    const G: f64 = 1024.0 * 1024.0 * 1024.0;
    const M: f64 = 1024.0 * 1024.0;
    if bytes as f64 >= G {
        format!("{:.1}GB", bytes as f64 / G)
    } else {
        format!("{:.0}MB", bytes as f64 / M)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Dir;

    fn meta(id: &str, tags: &[&str]) -> ImageMeta {
        ImageMeta {
            id: id.into(),
            fingerprint: format!("fp{id}"),
            tags: tags.iter().map(|t| t.to_string()).collect(),
            api: 35,
            flavour: "google_apis".into(),
            abi: "arm64-v8a".into(),
            system_image: "system-images;android-35;google_apis;arm64-v8a".into(),
            system_image_revision: "9".into(),
            emulator_version: "1.0".into(),
            memory_mb: 2048,
            cpus: 2,
            disk_mb: 4096,
            device: "pixel_7".into(),
            width: 1080,
            height: 2400,
            headless: true,
            gpu: "swiftshader_indirect".into(),
            snapshot: Some("ready".into()),
            launch: None,
            created_at: 1,
        }
    }

    fn put(store: &Store, m: &ImageMeta) {
        fs::create_dir_all(image_dir(store, &m.id)).unwrap();
        save_meta(store, m).unwrap();
    }

    #[test]
    fn tag_syntax() {
        assert_eq!(normalize_tag("myapp").unwrap(), "myapp:latest");
        assert_eq!(normalize_tag("org/myapp:v1.2").unwrap(), "org/myapp:v1.2");
        assert!(normalize_tag("MyApp").is_err());
        assert!(normalize_tag("a:b:c").is_err());
        assert!(normalize_tag("").is_err());
    }

    #[test]
    fn find_by_tag_and_id_and_retag() {
        let d = Dir::new();
        let store = Store::at(d.path().to_path_buf()).unwrap();
        let mut a = meta("aaaa11112222", &["myapp:latest"]);
        let mut b = meta("bbbb11112222", &[]);
        put(&store, &a);
        put(&store, &b);
        assert_eq!(find(&store, "myapp").unwrap().id, a.id);
        assert_eq!(find(&store, "bbbb").unwrap().id, b.id);
        assert!(find(&store, "nope").is_err());
        // Moving the tag to b removes it from a.
        apply_tag(&store, &mut b, "myapp:latest").unwrap();
        assert_eq!(find(&store, "myapp").unwrap().id, b.id);
        a = find(&store, "aaaa").unwrap();
        assert!(a.tags.is_empty());
    }

    #[test]
    fn remove_is_scoped() {
        let d = Dir::new();
        let store = Store::at(d.path().to_path_buf()).unwrap();
        let m = meta("cccc11112222", &[]);
        put(&store, &m);
        remove(&store, &m).unwrap();
        assert!(list(&store).unwrap().is_empty());
        let mut evil = m.clone();
        evil.id = "../instances".into();
        assert!(remove(&store, &evil).is_err());
        assert!(d.path().join("instances").is_dir());
    }

    #[test]
    fn usage_counts_files() {
        let d = Dir::new();
        fs::write(d.path().join("a"), vec![1u8; 10_000]).unwrap();
        let (logical, allocated) = dir_usage(d.path());
        assert_eq!(logical, 10_000);
        assert!(allocated >= 10_000);
    }
}
