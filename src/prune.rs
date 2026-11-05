//! `dorker prune`: reclaim disk by removing unused images, stopped instances and leftovers.
//!
//! Safe by default: only untagged images that no instance was made from are removed. Everything
//! else needs a flag, the plan is always shown first, and nothing is deleted without
//! confirmation (a terminal prompt, or `--yes`).

use crate::errors;
use crate::images::{self, ImageMeta, dir_usage, human};
use crate::runtime;
use crate::store::{Instance, State, Store, now_secs};
use anyhow::{Result, bail};
use std::collections::HashSet;
use std::path::PathBuf;

pub struct PruneArgs {
    pub all: bool,
    pub instances: bool,
    pub older_than: Option<String>,
    pub dry_run: bool,
    pub yes: bool,
    pub json: bool,
}

/// How many images were kept, and why.
#[derive(Debug, PartialEq, Eq, Default)]
pub struct Skipped {
    pub in_use: usize,
    pub tagged: usize,
    pub too_recent: usize,
}

/// A `Created` record this young may be a `dorker run` that is still starting.
const STARTING_GRACE_SECS: u64 = 120;
/// A `.partial` directory this young may be a build publishing right now.
const PARTIAL_GRACE_SECS: u64 = 600;

/// `30m`, `12h`, `7d` or `2w` in seconds.
pub fn parse_age(s: &str) -> Result<u64> {
    let t = s.trim();
    let split = t.find(|c: char| !c.is_ascii_digit()).unwrap_or(t.len());
    let (num, unit) = t.split_at(split);
    let n: u64 = num.parse().map_err(|_| {
        anyhow::anyhow!("invalid duration {s:?}: use a number and a unit like 30m, 12h, 7d or 2w")
    })?;
    let mult = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        "w" => 604_800,
        _ => bail!("invalid duration {s:?}: use a number and a unit like 30m, 12h, 7d or 2w"),
    };
    Ok(n * mult)
}

pub fn human_age(secs: u64) -> String {
    let plural = |n: u64, unit: &str| format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" });
    match secs {
        0..=59 => "just now".to_string(),
        60..=3599 => plural(secs / 60, "minute"),
        3600..=86_399 => plural(secs / 3600, "hour"),
        _ => plural(secs / 86_400, "day"),
    }
}

/// Which images to remove. In use (an instance was made from it) always wins and is never
/// removed; tagged images need `all`; `older_than` keeps anything used more recently.
pub fn select_images<'a>(
    images: &'a [ImageMeta],
    used: &HashSet<String>,
    all: bool,
    older_than: Option<u64>,
    now: u64,
) -> (Vec<&'a ImageMeta>, Skipped) {
    let mut chosen = Vec::new();
    let mut skipped = Skipped::default();
    for m in images {
        if used.contains(&m.id) {
            skipped.in_use += 1;
        } else if !m.tags.is_empty() && !all {
            skipped.tagged += 1;
        } else if older_than.is_some_and(|age| now.saturating_sub(m.last_used_or_created()) < age) {
            skipped.too_recent += 1;
        } else {
            chosen.push(m);
        }
    }
    (chosen, skipped)
}

/// Instances that are not running: stopped, failed, crashed, or a stale half-created record.
/// `older_than` applies to when the instance was created.
pub fn select_instances(
    instances: &[Instance],
    older_than: Option<u64>,
    now: u64,
) -> Vec<&Instance> {
    instances
        .iter()
        .filter(|i| match i.state {
            State::Stopped | State::Failed | State::Crashed => true,
            State::Created => now.saturating_sub(i.created_at) > STARTING_GRACE_SECS,
            _ => false,
        })
        .filter(|i| older_than.is_none_or(|age| now.saturating_sub(i.created_at) >= age))
        .collect()
}

/// Leftovers of interrupted builds: `images/.<id>.partial` directories that are not brand new.
fn incomplete_builds(store: &Store) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(store.images_dir()) else {
        return vec![];
    };
    rd.flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with('.'))
        .filter(|e| {
            e.metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age.as_secs() >= PARTIAL_GRACE_SECS)
        })
        .map(|e| e.path())
        .collect()
}

fn ask_yes_no(question: &str) -> bool {
    use std::io::{BufRead, Write};
    eprint!("{question} [y/N] ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line).unwrap_or(0) == 0 {
        return false;
    }
    matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

struct ImageRow<'a> {
    meta: &'a ImageMeta,
    bytes: u64,
}

struct InstanceRow<'a> {
    inst: &'a Instance,
    bytes: u64,
}

pub fn run(a: PruneArgs) -> Result<i32> {
    let cfg = errors::with(errors::CONFIG);
    let older_than = a
        .older_than
        .as_deref()
        .map(parse_age)
        .transpose()
        .map_err(cfg)?;
    if a.json && !a.yes && !a.dry_run {
        return Err(cfg(anyhow::anyhow!(
            "--json is non-interactive: add --yes to delete, or --dry-run to preview"
        )));
    }
    let store = Store::open()?;
    let now = now_secs();

    let all_images = images::list(&store)?;
    let mut all_instances = store.load_all()?;
    for i in &mut all_instances {
        runtime::reconcile(&store, i)?; // a dead process becomes CRASHED before we judge it
    }
    // Images an instance was made from are kept: they are the cache for that environment.
    let used: HashSet<String> = all_instances
        .iter()
        .filter_map(|i| i.image_id.clone())
        .collect();

    let (chosen, skipped) = select_images(&all_images, &used, a.all, older_than, now);
    let image_rows: Vec<ImageRow> = chosen
        .into_iter()
        .map(|meta| ImageRow {
            meta,
            bytes: dir_usage(&images::image_dir(&store, &meta.id)).1,
        })
        .collect();
    let inst_rows: Vec<InstanceRow> = if a.instances {
        select_instances(&all_instances, older_than, now)
            .into_iter()
            .map(|inst| InstanceRow {
                inst,
                // What deleting the instance frees (instances are clones of their image).
                bytes: images::dir_private_usage(&store.instance_dir(&inst.id)),
            })
            .collect()
    } else {
        vec![]
    };
    let partials = incomplete_builds(&store);
    let partial_bytes: u64 = partials.iter().map(|p| dir_usage(p).1).sum();
    let total: u64 = image_rows.iter().map(|r| r.bytes).sum::<u64>()
        + inst_rows.iter().map(|r| r.bytes).sum::<u64>()
        + partial_bytes;
    let nothing = image_rows.is_empty() && inst_rows.is_empty() && partials.is_empty();

    if a.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&plan_json(
                &a,
                &image_rows,
                &inst_rows,
                &partials,
                total,
                &skipped,
                now
            ))?
        );
    } else {
        print_plan(
            &image_rows,
            &inst_rows,
            &partials,
            total,
            &skipped,
            now,
            nothing,
            a.all,
            a.instances,
        );
    }
    if nothing || a.dry_run {
        return Ok(0);
    }

    if !a.yes {
        use std::io::IsTerminal;
        if !(std::io::stdin().is_terminal() && std::io::stderr().is_terminal()) {
            return Err(cfg(anyhow::anyhow!(
                "refusing to delete without confirmation and nothing was removed.\nRe-run with --yes, or use --dry-run to preview."
            )));
        }
        if !ask_yes_no("Remove these?") {
            eprintln!("Nothing removed.");
            return Ok(0);
        }
    }

    let free_before = runtime::free_space_bytes(&store.root());
    let mut failures = 0;
    {
        // Serialise with `run`/`build`, which clone and publish images under this lock.
        let _lock = store.lock_create()?;
        for r in &image_rows {
            if let Err(e) = images::remove(&store, r.meta) {
                eprintln!("error: could not remove image {}: {e:#}", r.meta.id);
                failures += 1;
            }
        }
        for p in &partials {
            if p.parent() != Some(store.images_dir().as_path())
                || std::fs::remove_dir_all(p).is_err()
            {
                eprintln!("error: could not remove {}", p.display());
                failures += 1;
            }
        }
    }
    for r in &inst_rows {
        let result = store
            .lock_instance(&r.inst.id, std::time::Duration::from_secs(10))
            .and_then(|_guard| store.remove(r.inst));
        if let Err(e) = result {
            eprintln!("error: could not remove instance {}: {e:#}", r.inst.name);
            failures += 1;
        }
    }
    let freed = free_before
        .zip(runtime::free_space_bytes(&store.root()))
        .map(|(b, af)| af.saturating_sub(b));
    if !a.json {
        println!(
            "Removed {} image(s), {} instance(s), {} incomplete build(s); freed {}.",
            image_rows.len(),
            inst_rows.len(),
            partials.len(),
            human(freed.unwrap_or(total))
        );
    }
    Ok(if failures == 0 { 0 } else { 1 })
}

fn age_of(m: &ImageMeta, now: u64) -> String {
    human_age(now.saturating_sub(m.last_used_or_created()))
}

fn plan_json(
    a: &PruneArgs,
    images: &[ImageRow],
    instances: &[InstanceRow],
    partials: &[PathBuf],
    total: u64,
    skipped: &Skipped,
    now: u64,
) -> serde_json::Value {
    serde_json::json!({
        "dry_run": a.dry_run,
        "images": images.iter().map(|r| serde_json::json!({
            "id": r.meta.id, "tags": r.meta.tags, "api": r.meta.api,
            "allocated_bytes": r.bytes, "last_used_secs_ago": now.saturating_sub(r.meta.last_used_or_created()),
        })).collect::<Vec<_>>(),
        "instances": instances.iter().map(|r| serde_json::json!({
            "id": r.inst.id, "name": r.inst.name, "status": r.inst.state.as_str(), "allocated_bytes": r.bytes,
        })).collect::<Vec<_>>(),
        "incomplete_builds": partials,
        "reclaimable_bytes": total,
        "kept": { "in_use": skipped.in_use, "tagged": skipped.tagged, "too_recent": skipped.too_recent },
    })
}

#[allow(clippy::too_many_arguments)]
fn print_plan(
    images: &[ImageRow],
    instances: &[InstanceRow],
    partials: &[PathBuf],
    total: u64,
    skipped: &Skipped,
    now: u64,
    nothing: bool,
    all: bool,
    with_instances: bool,
) {
    let st = crate::style::Style::stdout();
    if nothing {
        println!("{}", st.green("Nothing to prune."));
    } else {
        if !images.is_empty() {
            println!("{}", st.bold("Images to remove:"));
            println!(
                "{}",
                st.dim(&format!(
                    "  {:<14}{:<24}{:<9}{:<10}LAST USED",
                    "IMAGE ID", "TAGS", "ANDROID", "SIZE"
                ))
            );
            for r in images {
                let tags = if r.meta.tags.is_empty() {
                    "<none>".to_string()
                } else {
                    r.meta.tags.join(",")
                };
                let tags = format!("{tags:<24}");
                let tags = if r.meta.tags.is_empty() {
                    st.dim(&tags)
                } else {
                    st.green_bold(&tags)
                };
                println!(
                    "  {}{}{:<9}{}{}",
                    st.cyan(&format!("{:<14}", r.meta.id)),
                    tags,
                    r.meta.api,
                    st.yellow(&format!("{:<10}", human(r.bytes))),
                    st.dim(&age_of(r.meta, now))
                );
            }
        }
        if !instances.is_empty() {
            println!("{}", st.bold("Instances to remove:"));
            println!(
                "{}",
                st.dim(&format!("  {:<14}{:<24}{:<10}SIZE", "ID", "NAME", "STATUS"))
            );
            for r in instances {
                println!(
                    "  {}{}{}{}",
                    st.cyan(&format!("{:<14}", r.inst.id)),
                    st.bold(&format!("{:<24}", r.inst.name)),
                    st.status(
                        r.inst.state.as_str(),
                        &format!("{:<10}", r.inst.state.as_str())
                    ),
                    st.yellow(&human(r.bytes))
                );
            }
        }
        if !partials.is_empty() {
            println!("Incomplete builds to remove: {}", partials.len());
        }
        println!(
            "\nThis frees up to {} (images share disk blocks with instances made from them, so the real amount can be less).",
            human(total)
        );
    }
    let mut kept = Vec::new();
    if skipped.in_use > 0 {
        kept.push(format!("{} used by instances", skipped.in_use));
    }
    if skipped.tagged > 0 && !all {
        kept.push(format!(
            "{} tagged (add --all to include them)",
            skipped.tagged
        ));
    }
    if skipped.too_recent > 0 {
        kept.push(format!("{} used too recently", skipped.too_recent));
    }
    if !kept.is_empty() {
        println!("Kept: {}.", kept.join(", "));
    }
    if !with_instances && !nothing {
        println!("Stopped instances are kept unless you add --instances.");
    }
    if !nothing {
        println!();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(id: &str, tags: &[&str], last_used_ago: u64, now: u64) -> ImageMeta {
        ImageMeta {
            id: id.into(),
            fingerprint: id.into(),
            tags: tags.iter().map(|t| t.to_string()).collect(),
            api: 35,
            flavour: "google_apis".into(),
            abi: "arm64-v8a".into(),
            system_image: "system-images;android-35;google_apis;arm64-v8a".into(),
            system_image_revision: "9".into(),
            emulator_version: "1".into(),
            memory_mb: 2048,
            cpus: 2,
            disk_mb: 4096,
            device: "pixel_7".into(),
            width: 1080,
            height: 2400,
            headless: true,
            gpu: "swiftshader_indirect".into(),
            snapshot: None,
            launch: None,
            created_at: now - last_used_ago - 1000,
            last_used: Some(now - last_used_ago),
        }
    }

    fn inst(state: State, created_ago: u64, now: u64) -> Instance {
        Instance {
            id: "i".into(),
            name: "i".into(),
            image: "x".into(),
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
            created_at: now - created_ago,
            device: "pixel_7".into(),
            width: 1080,
            height: 2400,
            gpu: "swiftshader_indirect".into(),
            image_id: None,
            snapshot: None,
        }
    }

    const NOW: u64 = 1_000_000_000;

    #[test]
    fn durations_parse_and_ages_read_naturally() {
        assert_eq!(parse_age("30m").unwrap(), 1800);
        assert_eq!(parse_age("12h").unwrap(), 43_200);
        assert_eq!(parse_age("7d").unwrap(), 604_800);
        assert_eq!(parse_age("2w").unwrap(), 1_209_600);
        for bad in ["", "7", "d", "7x", "-1d", "1.5d"] {
            assert!(parse_age(bad).is_err(), "{bad:?}");
        }
        assert_eq!(human_age(5), "just now");
        assert_eq!(human_age(60), "1 minute ago");
        assert_eq!(human_age(7200), "2 hours ago");
        assert_eq!(human_age(86_400), "1 day ago");
        assert_eq!(human_age(5 * 86_400), "5 days ago");
    }

    #[test]
    fn default_selection_is_untagged_and_unused_only() {
        let imgs = vec![
            img("untagged-old", &[], 10 * 86_400, NOW),
            img("tagged", &["app:latest"], 10 * 86_400, NOW),
            img("in-use", &[], 10 * 86_400, NOW),
            img("in-use-tagged", &["x:1"], 10 * 86_400, NOW),
        ];
        let used: HashSet<String> = ["in-use", "in-use-tagged"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (chosen, skipped) = select_images(&imgs, &used, false, None, NOW);
        assert_eq!(
            chosen.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["untagged-old"]
        );
        assert_eq!(
            skipped,
            Skipped {
                in_use: 2,
                tagged: 1,
                too_recent: 0
            }
        );
    }

    #[test]
    fn all_includes_tagged_images_but_never_ones_in_use() {
        let imgs = vec![
            img("tagged", &["app:latest"], 100, NOW),
            img("in-use-tagged", &["x:1"], 100, NOW),
            img("plain", &[], 100, NOW),
        ];
        let used: HashSet<String> = ["in-use-tagged".to_string()].into();
        let (chosen, skipped) = select_images(&imgs, &used, true, None, NOW);
        let ids: Vec<&str> = chosen.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["tagged", "plain"]);
        assert_eq!(skipped.in_use, 1);
    }

    #[test]
    fn older_than_keeps_recently_used_images() {
        let imgs = vec![
            img("old", &[], 8 * 86_400, NOW),
            img("fresh", &[], 3600, NOW),
            img("edge", &[], 7 * 86_400, NOW),
        ];
        let (chosen, skipped) = select_images(&imgs, &HashSet::new(), false, Some(7 * 86_400), NOW);
        let ids: Vec<&str> = chosen.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["old", "edge"], "exactly 7 days counts as old enough");
        assert_eq!(skipped.too_recent, 1);
        // Images from before last_used was tracked fall back to their creation time.
        let mut legacy = img("legacy", &[], 0, NOW);
        legacy.last_used = None;
        legacy.created_at = NOW - 30 * 86_400;
        assert_eq!(
            select_images(&[legacy], &HashSet::new(), false, Some(86_400), NOW)
                .0
                .len(),
            1
        );
    }

    #[test]
    fn instances_are_only_selected_when_not_running_and_not_starting() {
        let set = vec![
            inst(State::Stopped, 10, NOW),
            inst(State::Failed, 10, NOW),
            inst(State::Crashed, 10, NOW),
            inst(State::Ready, 10_000, NOW),
            inst(State::Booting, 10_000, NOW),
            inst(State::Starting, 10_000, NOW),
            inst(State::Created, 30, NOW), // a run that is still starting
            inst(State::Created, 3600, NOW), // a stale half-created record
        ];
        let states: Vec<&str> = select_instances(&set, None, NOW)
            .iter()
            .map(|i| i.state.as_str())
            .collect();
        assert_eq!(states, ["STOPPED", "FAILED", "CRASHED", "CREATED"]);
        // --older-than applies to when the instance was created.
        let aged = vec![
            inst(State::Stopped, 100, NOW),
            inst(State::Stopped, 10 * 86_400, NOW),
        ];
        assert_eq!(select_instances(&aged, Some(86_400), NOW).len(), 1);
    }

    #[test]
    fn json_requires_an_explicit_choice_and_bad_durations_are_config_errors() {
        let a = |json, yes, dry, older: Option<&str>| PruneArgs {
            all: false,
            instances: false,
            older_than: older.map(String::from),
            dry_run: dry,
            yes,
            json,
        };
        let e = run(a(true, false, false, None)).unwrap_err();
        assert_eq!(errors::code_of(&e), errors::CONFIG);
        assert!(e.to_string().contains("--yes") && e.to_string().contains("--dry-run"));
        let e = run(a(false, true, false, Some("soon"))).unwrap_err();
        assert_eq!(errors::code_of(&e), errors::CONFIG);
    }
}
