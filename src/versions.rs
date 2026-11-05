//! `dorker versions`: which Android versions can be used, installed or available to `pull`.

use crate::errors;
use crate::sdk::{RemoteImage, Sdk, SystemImage, flavour_rank, host_abi};
use crate::style::Style;
use anyhow::Result;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Flavour {
    pub name: String,
    pub installed: bool,
}

/// One Android release (`android-37.0`, `android-36`, ...) and the flavours it comes in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub api: u32,
    pub minor: Option<u32>,
    /// What to type after `FROM` / `dorker pull`, e.g. `android:37`.
    pub reference: String,
    pub flavours: Vec<Flavour>,
}

impl Row {
    pub fn installed(&self) -> bool {
        self.flavours.iter().any(|f| f.installed)
    }
}

/// Flavours sort phone flavours by dorker's preference, then the rest alphabetically.
fn flavour_order(name: &str) -> (u8, String) {
    (flavour_rank(name).unwrap_or(99), name.to_string())
}

/// Merge what Google publishes with what is installed into one row per release, newest first.
/// `all` keeps non-phone flavours (Wear, TV, tablet, automotive...). Pure, for tests.
pub fn build_rows(
    remote: &[RemoteImage],
    installed: &[SystemImage],
    abi: &str,
    all: bool,
    api: Option<u32>,
) -> Vec<Row> {
    let mut map: BTreeMap<(u32, Option<u32>), BTreeMap<String, bool>> = BTreeMap::new();
    for r in remote.iter().filter(|r| r.abi == abi) {
        map.entry((r.api, r.minor))
            .or_default()
            .entry(r.flavour.clone())
            .or_insert(false);
    }
    for i in installed.iter().filter(|i| i.abi == abi) {
        map.entry((i.api, i.minor))
            .or_default()
            .insert(i.flavour.clone(), true);
    }
    let plain_exists = |api: u32| map.contains_key(&(api, None));
    let mut rows: Vec<Row> = map
        .iter()
        .filter(|((a, _), _)| api.is_none_or(|want| want == *a))
        .filter_map(|(&(a, minor), flavours)| {
            let mut fl: Vec<Flavour> = flavours
                .iter()
                .filter(|(name, installed)| all || **installed || flavour_rank(name).is_some())
                .map(|(name, installed)| Flavour {
                    name: name.clone(),
                    installed: *installed,
                })
                .collect();
            if fl.is_empty() {
                return None;
            }
            fl.sort_by_key(|f| flavour_order(&f.name));
            let reference = match minor {
                None => format!("android:{a}"),
                // `android:37` means the base release `37.0` unless a plain `android-37` exists.
                Some(0) if !plain_exists(a) => format!("android:{a}"),
                Some(m) => format!("android:{a}.{m}"),
            };
            Some(Row {
                api: a,
                minor,
                reference,
                flavours: fl,
            })
        })
        .collect();
    rows.sort_by_key(|r| std::cmp::Reverse((r.api, r.minor.unwrap_or(0))));
    rows
}

/// Wrap `items` (comma separated) into lines of at most `width` characters.
fn wrap(items: &[String], width: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for (i, item) in items.iter().enumerate() {
        let piece = if i + 1 < items.len() {
            format!("{item}, ")
        } else {
            item.clone()
        };
        if !cur.is_empty() && cur.chars().count() + piece.chars().count() > width {
            lines.push(cur.trim_end().to_string());
            cur = String::new();
        }
        cur.push_str(&piece);
    }
    if !cur.is_empty() {
        lines.push(cur.trim_end().to_string());
    }
    lines
}

/// The human-readable table. Padding is applied before colouring so columns stay aligned.
pub fn render(rows: &[Row], st: &Style, remote_known: bool) -> String {
    let use_w = rows
        .iter()
        .map(|r| r.reference.len())
        .max()
        .unwrap_or(0)
        .max("USE".len());
    let mut out = format!(
        "{:<use_w$}  {:<10}  FLAVOURS (* = installed)\n",
        "USE", "STATUS"
    );
    for r in rows {
        let status = if r.installed() {
            st.green(&format!("{:<10}", "installed"))
        } else {
            format!("{:<10}", "available")
        };
        let names: Vec<String> = r
            .flavours
            .iter()
            .map(|f| {
                if f.installed {
                    format!("{}*", f.name)
                } else {
                    f.name.clone()
                }
            })
            .collect();
        let lines = wrap(&names, 64);
        for (i, line) in lines.iter().enumerate() {
            if i == 0 {
                out.push_str(&format!(
                    "{}  {}  {}\n",
                    st.bold(&format!("{:<use_w$}", r.reference)),
                    status,
                    line
                ));
            } else {
                out.push_str(&format!("{:<use_w$}  {:<10}  {}\n", "", "", line));
            }
        }
    }
    if rows.is_empty() {
        out.push_str("(no matching Android versions)\n");
    }
    out.push('\n');
    out.push_str(
        &st.dim("Use one as `FROM android:<version>` or `dorker pull android:<version>`."),
    );
    out.push('\n');
    out.push_str(
        &st.dim("`pull` installs the first flavour listed; add `-<flavour>` to choose another,"),
    );
    out.push('\n');
    out.push_str(&st.dim("e.g. android:37-google_apis_playstore."));
    out.push('\n');
    if !remote_known {
        out.push_str(&st.yellow(
            "note: could not read Google's repository (offline, or no `android` CLI), so only installed images are shown.",
        ));
        out.push('\n');
    }
    out
}

pub fn run(api: Option<u32>, all: bool, json: bool) -> Result<i32> {
    let sdk = Sdk::locate().map_err(errors::with(errors::DEPENDENCY))?;
    let abi = host_abi();
    if !json {
        eprintln!("Reading Google's SDK repository...");
    }
    let pattern = api.map_or_else(
        || "system-images/*".to_string(),
        |a| format!("system-images/android-{a}*"),
    );
    let remote = sdk.list_remote_images(&pattern);
    let rows = build_rows(
        remote.as_deref().unwrap_or(&[]),
        &sdk.system_images(),
        abi,
        all,
        api,
    );
    if json {
        let out: Vec<_> = rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "api": r.api,
                    "minor": r.minor,
                    "use": r.reference,
                    "installed": r.installed(),
                    "pull": format!("dorker pull {}", r.reference),
                    "flavours": r.flavours.iter().map(|f| serde_json::json!({"name": f.name, "installed": f.installed})).collect::<Vec<_>>(),
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "abi": abi,
                "repository_read": remote.is_some(),
                "versions": out,
            }))?
        );
    } else {
        print!("{}", render(&rows, &Style::stdout(), remote.is_some()));
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sdk::parse_remote_images;
    use crate::style::strip_ansi;

    /// Real `android sdk list --all` lines for x86_64 across APIs 36 and 37 (plus previews).
    const LISTING: &str = "
  system-images/android-36/google_apis/x86_64                    7.0.0  Google APIs
  system-images/android-36/google_apis_playstore/x86_64          7.0.0  Google Play
  system-images/android-36/google_apis/arm64-v8a                 7.0.0  Google APIs
  system-images/android-36.1/google_apis_playstore/x86_64        1.0.0  Google Play
  system-images/android-37.0/android-wear-signed/x86_64          1.0.0  Wear OS
  system-images/android-37.0/aosp_atd/x86_64                     1.0.0  AOSP ATD
  system-images/android-37.0/google_apis/x86_64                  8.0.0  Google APIs
  system-images/android-37.0/google_apis_playstore/x86_64        8.0.0  Google Play
  system-images/android-37.0/google_apis_ps16k/x86_64            8.0.0  16 KB
  system-images/android-37.1/google_apis_ps16k/x86_64            9.0.0  16 KB
  system-images/android-37.2-beta1/google_apis_ps16k/x86_64      1.0.0  beta
  system-images/android-CANARY/google_apis_ps16k/x86_64          15.0.0 canary
";

    fn installed(api: u32, minor: Option<u32>, flavour: &str) -> SystemImage {
        SystemImage {
            api,
            minor,
            flavour: flavour.into(),
            abi: "x86_64".into(),
            sysdir: String::new(),
            package: format!("system-images;android-{api};{flavour};x86_64"),
        }
    }

    #[test]
    fn rows_are_newest_first_with_the_reference_to_type() {
        let rows = build_rows(&parse_remote_images(LISTING), &[], "x86_64", false, None);
        let refs: Vec<&str> = rows.iter().map(|r| r.reference.as_str()).collect();
        // 37.0 is "android:37" (no plain android-37 exists); previews never appear.
        assert_eq!(
            refs,
            ["android:37.1", "android:37", "android:36.1", "android:36"]
        );
        let base37 = rows.iter().find(|r| r.reference == "android:37").unwrap();
        let names: Vec<&str> = base37.flavours.iter().map(|f| f.name.as_str()).collect();
        // Phone flavours in dorker's preference order; Wear is hidden unless --all.
        assert_eq!(
            names,
            [
                "google_apis",
                "aosp_atd",
                "google_apis_ps16k",
                "google_apis_playstore"
            ]
        );
        assert!(rows.iter().all(|r| !r.installed()));
    }

    #[test]
    fn a_plain_major_keeps_the_dot_zero_reference_distinct() {
        let mut imgs = parse_remote_images(
            "  system-images/android-36/google_apis/x86_64 1 x\n  system-images/android-36.0/google_apis/x86_64 1 x",
        );
        imgs.sort_by(|a, b| a.path.cmp(&b.path));
        let rows = build_rows(&imgs, &[], "x86_64", false, None);
        let refs: Vec<&str> = rows.iter().map(|r| r.reference.as_str()).collect();
        assert!(
            refs.contains(&"android:36") && refs.contains(&"android:36.0"),
            "{refs:?}"
        );
    }

    #[test]
    fn all_adds_non_phone_flavours_and_the_api_filter_narrows() {
        let remote = parse_remote_images(LISTING);
        let all = build_rows(&remote, &[], "x86_64", true, None);
        let base37 = all.iter().find(|r| r.reference == "android:37").unwrap();
        assert!(
            base37
                .flavours
                .iter()
                .any(|f| f.name == "android-wear-signed")
        );
        let only36 = build_rows(&remote, &[], "x86_64", false, Some(36));
        assert!(only36.iter().all(|r| r.api == 36) && only36.len() == 2);
        assert!(build_rows(&remote, &[], "x86_64", false, Some(99)).is_empty());
        // The other ABI has its own (smaller) set.
        let arm = build_rows(&remote, &[], "arm64-v8a", false, None);
        assert_eq!(arm.len(), 1);
        assert_eq!(arm[0].reference, "android:36");
    }

    #[test]
    fn installed_images_are_marked_and_shown_even_when_offline() {
        let remote = parse_remote_images(LISTING);
        let inst = [
            installed(37, Some(0), "google_apis"),
            installed(35, None, "google_apis"),
        ];
        let rows = build_rows(&remote, &inst, "x86_64", false, None);
        let r37 = rows.iter().find(|r| r.reference == "android:37").unwrap();
        assert!(r37.installed());
        assert!(
            r37.flavours
                .iter()
                .find(|f| f.name == "google_apis")
                .unwrap()
                .installed
        );
        assert!(
            !r37.flavours
                .iter()
                .find(|f| f.name == "aosp_atd")
                .unwrap()
                .installed
        );
        // 35 is not in the repository listing at all but is installed: still listed.
        assert!(
            rows.iter()
                .any(|r| r.reference == "android:35" && r.installed())
        );
        // Offline: only the installed ones.
        let offline = build_rows(&[], &inst, "x86_64", false, None);
        assert_eq!(offline.len(), 2);
        // An installed non-phone flavour is shown even without --all (you have it).
        let odd = build_rows(
            &[],
            &[installed(34, None, "android-wear")],
            "x86_64",
            false,
            None,
        );
        assert_eq!(odd.len(), 1);
    }

    #[test]
    fn rendering_is_aligned_wrapped_and_colour_does_not_change_the_text() {
        let remote = parse_remote_images(LISTING);
        let inst = [installed(37, Some(0), "google_apis")];
        let rows = build_rows(&remote, &inst, "x86_64", true, None);
        let plain = render(&rows, &Style { on: false }, true);
        let styled = render(&rows, &Style { on: true }, true);
        assert!(!plain.contains('\x1b'));
        assert!(styled.contains("\x1b[32minstalled"), "installed is green");
        assert_eq!(
            strip_ansi(&styled),
            plain,
            "same text and alignment once colour is removed"
        );
        assert!(
            plain.starts_with("USE           STATUS      FLAVOURS (* = installed)\n"),
            "{plain}"
        );
        assert!(
            plain.contains("android:37    installed   google_apis*, "),
            "{plain}"
        );
        assert!(
            plain.contains("android:36    available   google_apis, google_apis_playstore"),
            "{plain}"
        );
        for line in plain.lines() {
            assert!(
                line.chars().count() <= 100,
                "line too wide ({}): {line}",
                line.chars().count()
            );
        }
        assert!(!plain.contains("could not read"));
        assert!(
            render(&rows, &Style { on: false }, false).contains("only installed images are shown")
        );
        assert!(render(&[], &Style { on: false }, true).contains("(no matching Android versions)"));
    }

    #[test]
    fn wrapping_keeps_items_whole() {
        let items: Vec<String> = ["aaaa", "bbbb", "cccc", "dddd"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(wrap(&items, 12), ["aaaa, bbbb,", "cccc, dddd"]);
        assert_eq!(wrap(&items, 100), ["aaaa, bbbb, cccc, dddd"]);
        assert!(wrap(&[], 10).is_empty());
    }
}
