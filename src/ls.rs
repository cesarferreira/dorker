//! `dorker ls`: images, instances and what they cost in disk space, in one view.

use crate::images::{self, ImageMeta, dir_usage, human};
use crate::prune::{human_age, select_images};
use crate::runtime;
use crate::sdk::Sdk;
use crate::store::{Instance, Store, now_secs};
use crate::style::Style;
use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

pub struct ImageLine {
    pub meta: ImageMeta,
    pub bytes: u64,
    /// Instances that were made from this image.
    pub used_by: usize,
}

pub struct InstanceLine {
    pub inst: Instance,
    pub bytes: u64,
}

pub struct SdkImages {
    pub root: PathBuf,
    pub count: usize,
    pub bytes: u64,
}

pub struct Report {
    pub store_root: PathBuf,
    pub images: Vec<ImageLine>,
    pub instances: Vec<InstanceLine>,
    /// What `dorker prune` (with no flags) would remove.
    pub reclaimable_images: usize,
    pub reclaimable_bytes: u64,
    pub free_bytes: Option<u64>,
    pub sdk: Option<SdkImages>,
}

impl Report {
    pub fn image_bytes(&self) -> u64 {
        self.images.iter().map(|i| i.bytes).sum()
    }
    pub fn instance_bytes(&self) -> u64 {
        self.instances.iter().map(|i| i.bytes).sum()
    }
}

/// Gather the report. Reads the store; sizes come from walking the directories.
pub fn collect(store: &Store) -> Result<Report> {
    let now = now_secs();
    let mut metas = images::list(store)?;
    metas.sort_by_key(|m| std::cmp::Reverse(m.last_used_or_created()));
    let mut instances = store.load_all()?;
    for i in &mut instances {
        runtime::reconcile(store, i)?; // a process that died becomes CRASHED before we show it
    }
    instances.sort_by_key(|i| std::cmp::Reverse(i.created_at));

    let mut used_by: HashMap<String, usize> = HashMap::new();
    for i in &instances {
        if let Some(id) = &i.image_id {
            *used_by.entry(id.clone()).or_default() += 1;
        }
    }
    let used: HashSet<String> = used_by.keys().cloned().collect();
    let (reclaimable, _) = select_images(&metas, &used, false, None, now);
    let reclaimable_images = reclaimable.len();
    let image_lines: Vec<ImageLine> = metas
        .iter()
        .map(|m| ImageLine {
            meta: m.clone(),
            bytes: dir_usage(&images::image_dir(store, &m.id)).1,
            used_by: used_by.get(&m.id).copied().unwrap_or(0),
        })
        .collect();
    let reclaimable_bytes: u64 = reclaimable
        .iter()
        .filter_map(|m| image_lines.iter().find(|l| l.meta.id == m.id))
        .map(|l| l.bytes)
        .sum();
    let instance_lines = instances
        .into_iter()
        // An instance's cost is what deleting it frees (its own data), not its apparent size: it
        // is a clone that shares most of its blocks with the image it came from.
        .map(|inst| InstanceLine {
            bytes: images::dir_private_usage(&store.instance_dir(&inst.id)),
            inst,
        })
        .collect();

    // The Android SDK's own system images are not dorker's, but they are the other big
    // consumer of disk, so show them for context when an SDK is found.
    let sdk = Sdk::locate().ok().map(|s| {
        let imgs = s.system_images();
        let bytes = imgs
            .iter()
            .map(|i| dir_usage(&s.root.join(i.sysdir.trim_end_matches('/'))).1)
            .sum();
        SdkImages {
            root: s.root.clone(),
            count: imgs.len(),
            bytes,
        }
    });

    Ok(Report {
        store_root: store.root(),
        images: image_lines,
        instances: instance_lines,
        reclaimable_images,
        reclaimable_bytes,
        free_bytes: runtime::free_space_bytes(&store.root()),
        sdk,
    })
}

fn used_by_text(n: usize) -> String {
    match n {
        0 => "-".to_string(),
        1 => "1 instance".to_string(),
        n => format!("{n} instances"),
    }
}

/// The human-readable report. Padding is applied before styling so columns stay aligned.
pub fn render(r: &Report, now: u64, st: &Style) -> String {
    let mut out = String::new();

    out.push_str(&st.bold("IMAGES"));
    out.push('\n');
    if r.images.is_empty() {
        out.push_str("  (none yet: `dorker build` or `dorker run` in a folder with a Dorkerfile creates one)\n");
    } else {
        out.push_str(&format!(
            "{:<14}{:<26}{:<9}{:<10}{:<10}{:<14}USED BY\n",
            "IMAGE ID", "TAGS", "ANDROID", "SNAPSHOT", "SIZE", "LAST USED"
        ));
        for l in &r.images {
            let tags = if l.meta.tags.is_empty() {
                "<none>".to_string()
            } else {
                l.meta.tags.join(",")
            };
            let tags = if tags.chars().count() > 24 {
                format!("{}...", tags.chars().take(21).collect::<String>())
            } else {
                tags
            };
            let age = human_age(now.saturating_sub(l.meta.last_used_or_created()));
            out.push_str(&format!(
                "{:<14}{:<26}{:<9}{:<10}{:<10}{:<14}{}\n",
                l.meta.id,
                tags,
                l.meta.api,
                l.meta.snapshot.as_deref().unwrap_or("-"),
                human(l.bytes),
                age,
                used_by_text(l.used_by)
            ));
        }
    }

    out.push('\n');
    out.push_str(&st.bold("INSTANCES"));
    out.push('\n');
    if r.instances.is_empty() {
        out.push_str("  (none)\n");
    } else {
        out.push_str(&format!(
            "{:<14}{:<22}{:<16}{:<10}OWN SIZE\n",
            "ID", "NAME", "IMAGE", "STATUS"
        ));
        for l in &r.instances {
            let status = format!("{:<10}", l.inst.state.as_str());
            let status = match l.inst.state.as_str() {
                "READY" => st.green(&status),
                "FAILED" | "CRASHED" => st.red_bold(&status),
                _ => status,
            };
            out.push_str(&format!(
                "{:<14}{:<22}{:<16}{}{}\n",
                l.inst.id,
                l.inst.name,
                l.inst.image.chars().take(14).collect::<String>(),
                status,
                human(l.bytes)
            ));
        }
    }

    out.push('\n');
    out.push_str(&st.bold(&format!("STORAGE  {}", r.store_root.display())));
    out.push('\n');
    let img = r.image_bytes();
    let inst = r.instance_bytes();
    out.push_str(&format!(
        "  {:<12}{:>3}   {:>8}",
        "Images",
        r.images.len(),
        human(img)
    ));
    if r.reclaimable_images > 0 {
        out.push_str(&format!(
            "   {} unused, {} reclaimable with `dorker prune`",
            r.reclaimable_images,
            human(r.reclaimable_bytes)
        ));
    }
    out.push('\n');
    out.push_str(&format!(
        "  {:<12}{:>3}   {:>8}",
        "Instances",
        r.instances.len(),
        human(inst)
    ));
    if !r.instances.is_empty() {
        out.push_str(if images::CLONE_AWARE_SIZES {
            "   only what they wrote; the rest is shared with their image"
        } else {
            "   full copies of their image (on btrfs/xfs they may share blocks, so this can overstate)"
        });
    }
    out.push('\n');
    out.push_str(&format!(
        "  {:<12}{:>3}   {:>8}   {}\n",
        "Total",
        "",
        human(img + inst),
        if images::CLONE_AWARE_SIZES {
            "images + what instances wrote"
        } else {
            "images + instances"
        }
    ));
    if let Some(free) = r.free_bytes {
        out.push_str(&format!(
            "  {:<12}{:>3}   {:>8}   free on this disk\n",
            "Free",
            "",
            human(free)
        ));
    }
    if let Some(s) = &r.sdk {
        out.push_str(&format!(
            "\n  Android SDK system images (not managed by dorker): {}, {}   {}\n",
            s.count,
            human(s.bytes),
            s.root.display()
        ));
    }
    if r.reclaimable_images > 0 {
        out.push('\n');
        out.push_str(&st.dim(&format!(
            "`dorker prune --dry-run` shows the {} image(s) that could be removed.",
            r.reclaimable_images
        )));
        out.push('\n');
    }
    out
}

pub fn json(r: &Report, now: u64) -> serde_json::Value {
    serde_json::json!({
        "store": r.store_root,
        "images": r.images.iter().map(|l| serde_json::json!({
            "id": l.meta.id, "tags": l.meta.tags, "api": l.meta.api, "flavour": l.meta.flavour,
            "snapshot": l.meta.snapshot, "allocated_bytes": l.bytes,
            "last_used_secs_ago": now.saturating_sub(l.meta.last_used_or_created()),
            "used_by_instances": l.used_by,
        })).collect::<Vec<_>>(),
        "instances": r.instances.iter().map(|l| serde_json::json!({
            "id": l.inst.id, "name": l.inst.name, "image": l.inst.image, "image_id": l.inst.image_id,
            "status": l.inst.state.as_str(), "own_bytes": l.bytes,
        })).collect::<Vec<_>>(),
        "storage": {
            "images_bytes": r.image_bytes(),
            "instances_bytes": r.instance_bytes(),
            "reclaimable_images": r.reclaimable_images,
            "reclaimable_bytes": r.reclaimable_bytes,
            "free_bytes": r.free_bytes,
            "clone_aware": images::CLONE_AWARE_SIZES,
            "note": "an instance's own_bytes is what deleting it frees; where clones are not detected (clone_aware=false) it is the full copy size and can overstate on btrfs/xfs",
            "android_sdk_system_images": r.sdk.as_ref().map(|s| serde_json::json!({
                "root": s.root, "count": s.count, "bytes": s.bytes,
            })),
        },
    })
}

pub fn run(as_json: bool) -> Result<i32> {
    let store = Store::open()?;
    let report = collect(&store)?;
    let now = now_secs();
    if as_json {
        println!("{}", serde_json::to_string_pretty(&json(&report, now))?);
    } else {
        print!("{}", render(&report, now, &Style::stdout()));
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::State;
    use crate::style::strip_ansi;

    const NOW: u64 = 1_000_000_000;

    fn meta(id: &str, tags: &[&str], used_ago: u64) -> ImageMeta {
        ImageMeta {
            id: id.into(),
            fingerprint: id.into(),
            tags: tags.iter().map(|t| t.to_string()).collect(),
            api: 35,
            flavour: "google_apis".into(),
            abi: "arm64-v8a".into(),
            system_image: "x".into(),
            system_image_revision: "1".into(),
            emulator_version: "1".into(),
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
            created_at: NOW - used_ago - 100,
            last_used: Some(NOW - used_ago),
        }
    }

    fn inst(name: &str, image_id: Option<&str>, state: State) -> Instance {
        Instance {
            id: format!("{name}-id"),
            name: name.into(),
            image: image_id.unwrap_or("android:35").into(),
            api: 35,
            flavour: "google_apis".into(),
            abi: "arm64-v8a".into(),
            memory_mb: 2048,
            cpus: 2,
            disk_mb: 4096,
            headless: true,
            auto_remove: false,
            console_port: 5554,
            pid: None,
            state,
            created_at: NOW - 50,
            device: "pixel_7".into(),
            width: 1080,
            height: 2400,
            gpu: "swiftshader_indirect".into(),
            image_id: image_id.map(String::from),
            snapshot: None,
        }
    }

    const GB: u64 = 1024 * 1024 * 1024;

    fn sample() -> Report {
        Report {
            store_root: PathBuf::from("/home/me/.dorker"),
            images: vec![
                ImageLine {
                    meta: meta("aaaaaaaaaaaa", &["app:latest"], 30),
                    bytes: 3 * GB,
                    used_by: 2,
                },
                ImageLine {
                    meta: meta("bbbbbbbbbbbb", &[], 3 * 86_400),
                    bytes: 4 * GB,
                    used_by: 0,
                },
            ],
            instances: vec![InstanceLine {
                inst: inst("web", Some("aaaaaaaaaaaa"), State::Ready),
                bytes: GB / 2,
            }],
            reclaimable_images: 1,
            reclaimable_bytes: 4 * GB,
            free_bytes: Some(200 * GB),
            sdk: Some(SdkImages {
                root: PathBuf::from("/sdk"),
                count: 3,
                bytes: 9 * GB,
            }),
        }
    }

    #[test]
    fn renders_images_instances_and_storage() {
        let text = render(&sample(), NOW, &Style { on: false });
        assert!(!text.contains('\x1b'));
        assert!(text.starts_with("IMAGES\nIMAGE ID      TAGS"), "{text}");
        assert!(text.contains("aaaaaaaaaaaa  app:latest"), "{text}");
        assert!(text.contains("2 instances"), "{text}");
        assert!(text.contains("3 days ago"), "{text}");
        let unused = text
            .lines()
            .find(|l| l.starts_with("bbbbbbbbbbbb"))
            .unwrap();
        assert!(
            unused.contains("<none>") && unused.trim_end().ends_with('-'),
            "an unused image shows '-': {unused}"
        );
        assert!(
            text.contains("INSTANCES\nID") && text.contains("web") && text.contains("READY"),
            "{text}"
        );
        assert!(text.contains("STORAGE  /home/me/.dorker"), "{text}");
        assert!(
            text.contains("Images")
                && text.contains("7.0GB")
                && text.contains("1 unused, 4.0GB reclaimable with `dorker prune`"),
            "{text}"
        );
        assert!(text.contains("OWN SIZE"), "{text}");
        assert!(
            text.contains("Total") && text.contains("7.5GB"),
            "images + what instances wrote: {text}"
        );
        assert!(
            text.contains(if images::CLONE_AWARE_SIZES {
                "only what they wrote"
            } else {
                "full copies of their image"
            }),
            "{text}"
        );
        assert!(
            text.contains("200.0GB") && text.contains("free on this disk"),
            "{text}"
        );
        assert!(
            text.contains("Android SDK system images (not managed by dorker): 3, 9.0GB"),
            "{text}"
        );
        assert!(text.contains("`dorker prune --dry-run`"), "{text}");
    }

    #[test]
    fn colour_does_not_change_the_text_and_empty_states_are_helpful() {
        let plain = render(&sample(), NOW, &Style { on: false });
        let styled = render(&sample(), NOW, &Style { on: true });
        assert!(styled.contains("\x1b[1mIMAGES\x1b[0m") && styled.contains("\x1b[32mREADY"));
        assert_eq!(strip_ansi(&styled), plain);
        let empty = Report {
            images: vec![],
            instances: vec![],
            reclaimable_images: 0,
            reclaimable_bytes: 0,
            free_bytes: None,
            sdk: None,
            ..sample()
        };
        let text = render(&empty, NOW, &Style { on: false });
        assert!(
            text.contains("(none yet: `dorker build` or `dorker run`"),
            "{text}"
        );
        assert!(text.contains("INSTANCES\n  (none)"), "{text}");
        assert!(
            !text.contains("reclaimable")
                && !text.contains("free on this disk")
                && !text.contains("Android SDK"),
            "{text}"
        );
    }

    #[test]
    fn long_tag_lists_are_shortened_so_columns_hold() {
        let mut r = sample();
        r.images[0].meta.tags = vec![
            "a-very-long-repository-name:latest".into(),
            "another:tag".into(),
        ];
        let text = render(&r, NOW, &Style { on: false });
        let line = text
            .lines()
            .find(|l| l.starts_with("aaaaaaaaaaaa"))
            .unwrap();
        assert!(line.contains("a-very-long-repositor..."), "{line}");
        assert!(line.contains("2 instances"));
    }

    #[test]
    fn json_has_per_image_usage_and_the_storage_summary() {
        let v = json(&sample(), NOW);
        assert_eq!(v["images"][0]["used_by_instances"], 2);
        assert_eq!(v["images"][1]["tags"], serde_json::json!([]));
        assert_eq!(v["instances"][0]["status"], "READY");
        assert_eq!(v["storage"]["images_bytes"], 7 * GB);
        assert_eq!(v["storage"]["reclaimable_bytes"], 4 * GB);
        assert_eq!(v["storage"]["android_sdk_system_images"]["count"], 3);
        assert!(v["storage"]["note"].as_str().unwrap().contains("own_bytes"));
        assert_eq!(v["instances"][0]["own_bytes"], GB / 2);
        assert_eq!(v["storage"]["clone_aware"], images::CLONE_AWARE_SIZES);
    }
}
