//! `dorker commit`: save a running instance as a new image.
//!
//! Some state can only be made by using the device (accepting a consent screen, switching on a
//! developer mode, signing in). `commit` keeps that state: it saves a snapshot of the running
//! instance, stops it, and copies its private AVD home into the image store. Instances started
//! from the new image resume from that snapshot, so they begin exactly where you left off.

use crate::commands::{OP_TIMEOUT, ready_instance_locked};
use crate::errors;
use crate::images::{self, ImageMeta};
use crate::runtime;
use crate::sdk::Sdk;
use crate::store::{State, Store, now_secs};
use crate::style::Style;
use crate::units::ImageRef;
use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::time::Duration;

/// Snapshot name used when the instance's image has none.
const DEFAULT_SNAPSHOT: &str = "committed";

/// The id of a committed image: unique per commit, derived from the instance and the time.
pub fn commit_id(parent: &str, instance_id: &str, now: u64) -> String {
    let mut h = Sha256::new();
    h.update(format!("commit\n{parent}\n{instance_id}\n{now}\n"));
    h.finalize()
        .iter()
        .take(6)
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub fn run(instance: &str, tag: Option<&str>, json: bool) -> Result<i32> {
    let tag = tag.map(images::normalize_tag).transpose()?;
    let sdk = Sdk::locate().map_err(errors::with(errors::DEPENDENCY))?;
    let store = Store::open()?;
    let found = store.find(instance)?;
    let _op = store.lock_instance(&found.id, OP_TIMEOUT)?;
    let mut inst = ready_instance_locked(&store, &found.id)?;

    let st = Style::stderr();
    let total = 4;
    let step = |n: usize, text: &str| eprintln!("{} {text}", st.cyan(&format!("[{n}/{total}]")));

    // The metadata of the new image: the parent image's when there is one, else resolved
    // from the system image the instance runs.
    let parent = inst
        .image_id
        .as_deref()
        .and_then(|id| images::find(&store, id).ok());
    let snapshot = inst
        .snapshot
        .clone()
        .or_else(|| parent.as_ref().and_then(|p| p.snapshot.clone()))
        .unwrap_or_else(|| DEFAULT_SNAPSHOT.to_string());
    let now = now_secs();
    let parent_key = parent.as_ref().map_or(inst.image.clone(), |p| p.id.clone());
    let id = commit_id(&parent_key, &inst.id, now);
    let mut meta = match &parent {
        Some(p) => p.clone(),
        None => {
            let sys = sdk
                .resolve(&ImageRef {
                    api: inst.api,
                    minor: None,
                    flavour: Some(inst.flavour.clone()),
                })
                .map_err(errors::with(errors::DEPENDENCY))?;
            ImageMeta {
                id: id.clone(),
                fingerprint: String::new(),
                tags: vec![],
                api: sys.api,
                flavour: sys.flavour.clone(),
                abi: sys.abi.clone(),
                system_image: sys.package.clone(),
                system_image_revision: sdk.system_image_revision(&sys)?,
                emulator_version: sdk.emulator_version()?,
                memory_mb: inst.memory_mb,
                cpus: inst.cpus,
                disk_mb: inst.disk_mb,
                device: inst.device.clone(),
                width: inst.width,
                height: inst.height,
                headless: inst.headless,
                gpu: inst.gpu.clone(),
                snapshot: None,
                launch: None,
                created_at: now,
                last_used: None,
            }
        }
    };
    meta.id = id.clone();
    meta.fingerprint = format!("commit:{}:{}:{now}", parent_key, inst.id);
    meta.tags = vec![];
    meta.snapshot = Some(snapshot.clone());
    meta.created_at = now;
    meta.last_used = None;
    // The new snapshot is of this instance's running state, so its hardware must match.
    meta.memory_mb = inst.memory_mb;
    meta.cpus = inst.cpus;
    meta.gpu = inst.gpu.clone();

    let avd = store.instance_dir(&inst.id).join("avd");
    runtime::ensure_free_space(
        &store.root(),
        images::dir_private_usage(&avd),
        "commit an image",
    )?;

    step(
        1,
        &format!("Saving a snapshot of {} ({snapshot:?})", inst.name),
    );
    // Flush the guest's file system first, so a cold boot of the image (or of this instance
    // later) also has everything that was written, not only the snapshot's RAM copy of it.
    crate::build::adb_checked(&sdk, &inst.serial(), &["shell", "sync"], "flushing to disk")?;
    let out = crate::build::adb_checked(
        &sdk,
        &inst.serial(),
        &["emu", "avd", "snapshot", "save", &snapshot],
        "saving the snapshot",
    )?;
    if out.lines().any(|l| l.starts_with("KO")) {
        bail!("the emulator refused to save the snapshot: {}", out.trim());
    }
    let snap_dir = avd
        .join(format!("{}.avd", runtime::AVD_NAME))
        .join("snapshots")
        .join(&snapshot);
    if !snap_dir.is_dir() {
        bail!(
            "the snapshot was not created (expected {})",
            snap_dir.display()
        );
    }

    step(
        2,
        "Stopping the instance (its files must not change while they are copied)",
    );
    inst.set_state(State::Stopping)?;
    store.save(&inst)?;
    runtime::kill_instance(&sdk, &inst, Duration::from_secs(20));
    inst.set_state(State::Stopped)?;
    inst.pid = None;
    store.save(&inst)?;

    step(3, "Copying the instance into a new image");
    let partial = store.images_dir().join(format!(".{id}.partial"));
    let _ = std::fs::remove_dir_all(&partial);
    std::fs::create_dir_all(&partial)?;
    let copied = runtime::clone_avd_home(&avd, &partial.join("avd"), meta.api).and_then(|()| {
        std::fs::write(
            partial.join("image.json"),
            serde_json::to_vec_pretty(&meta)?,
        )
        .context("writing image.json")
    });
    if let Err(e) = copied {
        let _ = std::fs::remove_dir_all(&partial);
        return Err(e);
    }
    // Emulator lock files from the stopped instance would make the next start think the AVD
    // is in use.
    remove_locks(&partial.join("avd"));
    std::fs::rename(&partial, images::image_dir(&store, &id)).context("publishing the image")?;

    step(4, "Done");
    if let Some(t) = &tag {
        images::apply_tag(&store, &mut meta, t)?;
    }
    let shown = tag.clone().unwrap_or_else(|| id.clone());
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "id": id, "tag": tag, "snapshot": snapshot,
                "instance": inst.name, "instance_status": inst.state.as_str(),
            }))?
        );
    } else {
        println!("{id}");
    }
    eprintln!(
        "Committed {} as image {shown}. The instance is stopped: `dorker start {}` boots it \
         again (cold boot, keeps its data), or `dorker run {shown}` makes a new one that resumes \
         from the saved state.",
        inst.name, inst.name
    );
    if inst.auto_remove {
        // A `run --rm` instance is removed when it stops; its state now lives in the image.
        store.remove(&inst)?;
    }
    Ok(0)
}

/// Delete `*.lock` files (emulator and hardware locks) anywhere under `dir`.
fn remove_locks(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            remove_locks(&p);
        } else if p.extension().is_some_and(|x| x == "lock") {
            let _ = std::fs::remove_file(&p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_ids_are_stable_for_the_same_inputs_and_differ_otherwise() {
        let a = commit_id("img", "inst", 100);
        assert_eq!(a.len(), 12);
        assert_eq!(a, commit_id("img", "inst", 100));
        assert_ne!(a, commit_id("img", "inst", 101));
        assert_ne!(a, commit_id("img", "other", 100));
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn lock_files_are_removed_recursively() {
        let d = crate::testutil::Dir::new();
        std::fs::create_dir_all(d.path().join("a/b")).unwrap();
        std::fs::write(d.path().join("a/b/x.lock"), b"").unwrap();
        std::fs::write(d.path().join("a/keep.img"), b"x").unwrap();
        std::fs::write(d.path().join("top.lock"), b"").unwrap();
        remove_locks(d.path());
        assert!(!d.path().join("a/b/x.lock").exists());
        assert!(!d.path().join("top.lock").exists());
        assert!(d.path().join("a/keep.img").exists());
    }
}
