//! Project templates: ready-made folders (a Dorkerfile plus helper scripts) that
//! `dorker init --template <name>` copies into a project. They live under `templates/` in
//! the repository and are embedded into the binary by `build.rs`.
//!
//! A template is a folder `templates/<name>/` holding:
//! - `template.toml`: `description = "..."` (required, one line) and any number of
//!   `next = "..."` lines (the steps shown after `init`);
//! - `README.md` (optional): shown by `dorker templates <name>`, not copied;
//! - every other file: copied as is (executable files stay executable).

use crate::errors;
use crate::style::Style;
use anyhow::Result;
use std::path::{Path, PathBuf};

include!(concat!(env!("OUT_DIR"), "/templates.rs"));

/// The template `dorker init` uses when none is named.
pub const DEFAULT: &str = "basic";

const META_FILE: &str = "template.toml";
const README_FILE: &str = "README.md";

pub struct Template {
    pub name: &'static str,
    pub description: String,
    pub next: Vec<String>,
    pub readme: Option<String>,
    /// Files to copy into a project (everything except the metadata and the README).
    pub files: Vec<&'static EmbeddedFile>,
}

/// Template names are lowercase words joined by hyphens, so they are safe in a path and a URL.
#[cfg(test)]
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 40
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--")
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Parse `template.toml`: only `description` and `next`, each as `key = "value"`.
pub fn parse_meta(text: &str) -> Result<(String, Vec<String>)> {
    let mut description = None;
    let mut next = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let at = |msg: &str| anyhow::anyhow!("{META_FILE} line {}: {msg}", i + 1);
        let Some((key, value)) = line.split_once('=') else {
            return Err(at("expected `key = \"value\"`"));
        };
        let value = value.trim();
        let Some(inner) = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .filter(|_| value.len() >= 2)
        else {
            return Err(at("the value must be in double quotes"));
        };
        let inner = inner.replace("\\\"", "\"").replace("\\\\", "\\");
        match key.trim() {
            "description" => {
                if description.replace(inner).is_some() {
                    return Err(at("`description` appears twice"));
                }
            }
            "next" => next.push(inner),
            other => {
                return Err(at(&format!(
                    "unknown key {other:?} (use description, next)"
                )));
            }
        }
    }
    let description = description
        .filter(|d| !d.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("{META_FILE} needs a `description = \"...\"` line"))?;
    Ok((description, next))
}

fn names() -> Vec<&'static str> {
    let mut v: Vec<&'static str> = EMBEDDED.iter().map(|f| f.template).collect();
    v.dedup();
    v
}

fn load(name: &'static str) -> Result<Template> {
    let own: Vec<&'static EmbeddedFile> = EMBEDDED.iter().filter(|f| f.template == name).collect();
    let text_of = |file: &str| {
        own.iter()
            .find(|f| f.path == file)
            .map(|f| String::from_utf8_lossy(f.data).into_owned())
    };
    let meta = text_of(META_FILE)
        .ok_or_else(|| anyhow::anyhow!("template {name:?} has no {META_FILE}"))?;
    let (description, next) =
        parse_meta(&meta).map_err(|e| anyhow::anyhow!("template {name:?}: {e}"))?;
    Ok(Template {
        name,
        description,
        next,
        readme: text_of(README_FILE),
        files: own
            .into_iter()
            .filter(|f| f.path != META_FILE && f.path != README_FILE)
            .collect(),
    })
}

/// Every template, sorted by name.
pub fn all() -> Result<Vec<Template>> {
    names().into_iter().map(load).collect()
}

/// Find a template by name; an unknown name lists the choices and the closest matches.
pub fn find(name: &str) -> Result<Template> {
    let known = names();
    if let Some(n) = known.iter().find(|n| **n == name) {
        return load(n);
    }
    let close: Vec<&str> = known
        .iter()
        .copied()
        .filter(|n| n.contains(name) || name.contains(n))
        .collect();
    let mut msg = format!("unknown template {name:?}");
    if !close.is_empty() {
        msg.push_str(&format!("\n\nDid you mean: {}?", close.join(", ")));
    }
    msg.push_str(&format!(
        "\n\nAvailable templates: {}\nRun `dorker templates` for descriptions.",
        known.join(", ")
    ));
    Err(errors::with(errors::CONFIG)(anyhow::anyhow!(msg)))
}

/// Everything wrong with the embedded templates (empty when they are all fine). Used by the
/// test suite, so a contributed template that is malformed fails CI.
#[cfg(test)]
pub fn problems() -> Vec<String> {
    let mut out = Vec::new();
    let known = names();
    if !known.contains(&DEFAULT) {
        out.push(format!("the default template {DEFAULT:?} is missing"));
    }
    for name in known {
        let say = |m: String| format!("template {name}: {m}");
        if !valid_name(name) {
            out.push(say(
                "the folder name must be lowercase words joined by hyphens".into(),
            ));
        }
        let t = match load(name) {
            Ok(t) => t,
            Err(e) => {
                out.push(format!("{e:#}"));
                continue;
            }
        };
        if t.description.len() > 120 {
            out.push(say("`description` is longer than 120 characters".into()));
        }
        if t.files.is_empty() {
            out.push(say("it has no files to copy".into()));
        }
        for f in &t.files {
            let bad_path = f
                .path
                .split('/')
                .any(|c| c.is_empty() || c == ".." || c == ".");
            if bad_path || f.path.starts_with('/') {
                out.push(say(format!("unsafe file path {:?}", f.path)));
            }
            let body = String::from_utf8_lossy(f.data);
            if f.path.ends_with(".sh") {
                if !body.starts_with("#!") {
                    out.push(say(format!("{} needs a `#!` line", f.path)));
                }
                if !f.executable {
                    out.push(say(format!("{} must be executable (chmod +x)", f.path)));
                }
            }
        }
        match t.files.iter().find(|f| f.path == "Dorkerfile") {
            None => out.push(say("it has no Dorkerfile".into())),
            Some(df) => {
                if let Err(e) =
                    crate::dorkerfile::parse("Dorkerfile", &String::from_utf8_lossy(df.data))
                {
                    out.push(say(format!("its Dorkerfile does not parse: {e}")));
                }
            }
        }
    }
    out
}

/// `dorker templates`: the list.
pub fn list(json: bool) -> Result<i32> {
    let all = all()?;
    if json {
        let rows: Vec<_> = all
            .iter()
            .map(|t| {
                serde_json::json!({
                    "name": t.name,
                    "description": t.description,
                    "files": t.files.iter().map(|f| f.path).collect::<Vec<_>>(),
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(0);
    }
    let st = Style::stdout();
    let width = all.iter().map(|t| t.name.len()).max().unwrap_or(0) + 2;
    println!("{}", st.dim(&format!("{:<width$}DESCRIPTION", "TEMPLATE")));
    for t in &all {
        let name = format!("{:<width$}", t.name);
        let default = if t.name == DEFAULT { "  (default)" } else { "" };
        println!(
            "{}{}{}",
            st.green_bold(&name),
            t.description,
            st.dim(default)
        );
    }
    println!(
        "\n{}",
        st.dim("Use one with `dorker init --template <name>`; see its files with `dorker templates <name>`.")
    );
    Ok(0)
}

/// `dorker templates <name>`: what one template contains.
pub fn show(name: &str, json: bool) -> Result<i32> {
    let t = find(name)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "name": t.name,
                "description": t.description,
                "files": t.files.iter().map(|f| f.path).collect::<Vec<_>>(),
                "next": t.next,
                "readme": t.readme,
            }))?
        );
        return Ok(0);
    }
    let st = Style::stdout();
    println!("{}  {}", st.green_bold(t.name), t.description);
    println!("\n{}", st.bold("Files"));
    for f in &t.files {
        let mark = if f.executable { " (executable)" } else { "" };
        println!("  {}{}", f.path, st.dim(mark));
    }
    if !t.next.is_empty() {
        println!("\n{}", st.bold("Then"));
        for n in &t.next {
            println!("  {n}");
        }
    }
    if let Some(readme) = &t.readme {
        println!("\n{}\n{}", st.bold("Notes"), readme.trim_end());
    }
    println!(
        "\n{}",
        st.dim(&format!(
            "Create it with `dorker init --template {}`.",
            t.name
        ))
    );
    Ok(0)
}

/// `dorker init [--template NAME] [DIR]`: copy a template into `dir` (default: here).
pub fn init(template: Option<&str>, dir: Option<&Path>, force: bool) -> Result<i32> {
    let t = find(template.unwrap_or(DEFAULT))?;
    let dir: PathBuf = dir.map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let targets: Vec<(PathBuf, &EmbeddedFile)> =
        t.files.iter().map(|f| (dir.join(f.path), *f)).collect();
    let existing: Vec<&PathBuf> = targets
        .iter()
        .map(|(p, _)| p)
        .filter(|p| p.exists())
        .collect();
    if !existing.is_empty() && !force {
        let list = existing
            .iter()
            .map(|p| format!("  {}", p.display()))
            .collect::<Vec<_>>()
            .join("\n");
        return Err(errors::with(errors::CONFLICT)(anyhow::anyhow!(
            "not overwriting existing files:\n{list}\n\nMove them, pick another folder (`dorker init --template {} <dir>`), or pass --force.",
            t.name
        )));
    }
    for (path, file) in &targets {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, file.data)?;
        #[cfg(unix)]
        if file.executable {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
        }
    }
    if t.name == DEFAULT && template.is_none() {
        println!("Created Dorkerfile.");
    } else {
        println!("Created from the {} template:", t.name);
        for (path, _) in &targets {
            println!("  {}", path.display());
        }
    }
    if !t.next.is_empty() {
        println!("\nNext:");
        for n in &t.next {
            println!("  {n}");
        }
    }
    println!(
        "\nEvery Dorkerfile directive: https://github.com/cesarferreira/dorker/blob/main/docs/dorkerfile.md\n`dorker templates` lists more templates."
    );
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_embedded_template_is_valid() {
        let problems = problems();
        assert!(problems.is_empty(), "{}", problems.join("\n"));
    }

    #[test]
    fn the_templates_we_ship_are_there() {
        let names = names();
        for n in [
            "basic",
            "android-auto",
            "android-auto-with-dev-mode",
            "playstore",
            "instrumentation-tests",
            "screenshots",
        ] {
            assert!(names.contains(&n), "{n} missing from {names:?}");
        }
    }

    #[test]
    fn template_names() {
        assert!(valid_name("android-auto"));
        assert!(valid_name("a1"));
        for bad in ["", "-a", "a-", "a--b", "A", "a_b", "a b", "../x", "a/b"] {
            assert!(!valid_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn metadata_parsing() {
        let (d, n) =
            parse_meta("# c\ndescription = \"Hi \\\"x\\\"\"\nnext = \"one\"\nnext = \"two\"\n")
                .unwrap();
        assert_eq!(d, "Hi \"x\"");
        assert_eq!(n, vec!["one", "two"]);
        assert!(
            parse_meta("next = \"x\"\n").is_err(),
            "description is required"
        );
        assert!(parse_meta("description = \"a\"\ndescription = \"b\"\n").is_err());
        assert!(parse_meta("description = a\n").is_err(), "unquoted");
        assert!(
            parse_meta("description = \"a\"\nnxt = \"b\"\n").is_err(),
            "typo'd key"
        );
        assert!(parse_meta("description = \"  \"\n").is_err());
        assert!(parse_meta("nonsense\n").is_err());
    }

    #[test]
    fn unknown_names_suggest_and_list() {
        let err = find("auto").err().unwrap().to_string();
        assert!(err.contains("Did you mean: android-auto"), "{err}");
        let err = find("zzz").err().unwrap().to_string();
        assert!(
            err.contains("Available templates:") && err.contains("basic"),
            "{err}"
        );
    }

    #[test]
    fn the_default_template_matches_what_init_always_wrote() {
        let t = find(DEFAULT).unwrap();
        let df = t.files.iter().find(|f| f.path == "Dorkerfile").unwrap();
        assert!(String::from_utf8_lossy(df.data).starts_with("FROM android:35"));
    }
}
