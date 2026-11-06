//! Dorkerfile parser. Pure: no filesystem access, so it is fully unit-tested.

use crate::units::{ImageRef, parse_size_mb};
use std::fmt;

const DIRECTIVES: [&str; 14] = [
    "FROM",
    "MEMORY",
    "CPUS",
    "DISK",
    "DEVICE",
    "RESOLUTION",
    "HEADLESS",
    "GPU",
    "INSTALL",
    "FETCH",
    "PROVISION",
    "SETTING",
    "SNAPSHOT",
    "CMD",
];

/// Emulator `-gpu` modes dorker accepts. `host` needs a GPU/display (fails on headless
/// Linux VMs); `swiftshader_indirect` works everywhere and is the headless default off macOS.
pub const GPU_MODES: [&str; 6] = [
    "auto",
    "swiftshader_indirect",
    "angle_indirect",
    "guest",
    "host",
    "off",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Setting {
    pub namespace: String,
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cmd {
    /// Launch the launcher activity of a package after startup.
    Launch(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dorkerfile {
    pub from: ImageRef,
    pub memory_mb: u64,
    pub cpus: u32,
    pub disk_mb: u64,
    pub device: String,
    pub resolution: (u32, u32),
    pub headless: bool,
    /// Emulator GPU mode; `None` means the default for the headless setting.
    pub gpu: Option<String>,
    /// APK paths exactly as written (relative to the build context).
    pub installs: Vec<String>,
    /// Host scripts that create missing `INSTALL` files (run only when one is missing).
    pub fetches: Vec<String>,
    /// Host scripts run against the build emulator, in order, exactly as written.
    pub provisions: Vec<String>,
    pub settings: Vec<Setting>,
    pub snapshot: Option<String>,
    pub cmd: Option<Cmd>,
}

impl Dorkerfile {
    /// The GPU mode to use: the explicit directive, else a safe default.
    pub fn gpu_mode(&self) -> String {
        self.gpu
            .clone()
            .unwrap_or_else(|| default_gpu(self.headless))
    }
}

/// The GPU mode used when none is chosen. A Mac always has a GPU, and rendering on it
/// uses far less CPU than software rendering (about 43% vs 600% while scrolling in our
/// measurements), so macOS defaults to `host` even when headless. Elsewhere a headless
/// machine may have no GPU or display (CI, VMs), so `swiftshader_indirect` is the default.
pub fn default_gpu(headless: bool) -> String {
    if let Some(g) = crate::config::current().gpu {
        return g;
    }
    platform_default_gpu(headless).to_string()
}

/// The built-in default, before the config file is consulted.
pub fn platform_default_gpu(headless: bool) -> &'static str {
    if !headless {
        "auto"
    } else if cfg!(target_os = "macos") {
        "host"
    } else {
        "swiftshader_indirect"
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub file: String,
    pub line: usize,
    pub col: usize,
    pub message: String,
    pub hint: Option<String>,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}:{}\nerror: {}",
            self.file, self.line, self.col, self.message
        )?;
        if let Some(h) = &self.hint {
            write!(f, "\n\n{h}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ParseError {}

struct Token {
    text: String,
    col: usize, // 1-based
}

/// Split a line into tokens. Supports "double" and 'single' quotes; a token
/// starting with `#` begins a comment.
fn tokenize(line: &str) -> Result<Vec<Token>, (usize, String)> {
    let chars: Vec<char> = line.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_whitespace() {
            i += 1;
            continue;
        }
        if chars[i] == '#' {
            break;
        }
        let col = i + 1;
        let mut text = String::new();
        while i < chars.len() && !chars[i].is_whitespace() {
            if chars[i] == '"' || chars[i] == '\'' {
                let quote = chars[i];
                let start = i + 1;
                i += 1;
                loop {
                    match chars.get(i) {
                        Some(&c) if c == quote => {
                            i += 1;
                            break;
                        }
                        Some(&c) => {
                            text.push(c);
                            i += 1;
                        }
                        None => return Err((start, "unterminated quoted string".into())),
                    }
                }
            } else {
                text.push(chars[i]);
                i += 1;
            }
        }
        out.push(Token { text, col });
    }
    Ok(out)
}

fn levenshtein(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            cur.push(
                (prev[j] + usize::from(ca != cb))
                    .min(prev[j + 1] + 1)
                    .min(cur[j] + 1),
            );
        }
        prev = cur;
    }
    prev[b.len()]
}

fn suggest(word: &str) -> Option<&'static str> {
    let up = word.to_ascii_uppercase();
    DIRECTIVES
        .iter()
        .find(|d| d.starts_with(&up) && up.len() >= 3)
        .copied()
        .or_else(|| {
            DIRECTIVES
                .iter()
                .min_by_key(|d| levenshtein(&up, d))
                .copied()
                .filter(|d| levenshtein(&up, d) <= 2)
        })
}

fn valid_package(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    parts.len() >= 2
        && parts.iter().all(|p| {
            p.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
                && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
}

fn safe_word(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-' | ':'))
}

pub fn parse(file: &str, source: &str) -> Result<Dorkerfile, ParseError> {
    let mut from: Option<(ImageRef, usize)> = None;
    let mut seen: std::collections::HashMap<&'static str, usize> = Default::default();
    let mut d = Dorkerfile {
        from: ImageRef {
            api: 0,
            minor: None,
            flavour: None,
        },
        memory_mb: 2048,
        cpus: 2,
        disk_mb: 4096,
        device: "pixel_7".into(),
        resolution: (1080, 2400),
        headless: true,
        gpu: None,
        installs: vec![],
        fetches: vec![],
        provisions: vec![],
        settings: vec![],
        snapshot: None,
        cmd: None,
    };

    for (idx, raw) in source.lines().enumerate() {
        let line = idx + 1;
        let err = |col: usize, message: String, hint: Option<String>| ParseError {
            file: file.to_string(),
            line,
            col,
            message,
            hint,
        };
        let tokens = tokenize(raw).map_err(|(col, m)| err(col, m, None))?;
        let Some(first) = tokens.first() else {
            continue;
        };
        let Some(&directive) = DIRECTIVES
            .iter()
            .find(|d| d.eq_ignore_ascii_case(&first.text))
        else {
            let hint = suggest(&first.text).map(|s| format!("Did you mean \"{s}\"?"));
            return Err(err(
                first.col,
                format!("unsupported directive \"{}\"", first.text),
                hint,
            ));
        };
        if from.is_none() && directive != "FROM" {
            return Err(err(
                first.col,
                format!("{directive} before FROM"),
                Some("FROM must be the first directive.".into()),
            ));
        }
        let args = &tokens[1..];
        let need = |n: usize| -> Result<(), ParseError> {
            if args.len() == n {
                Ok(())
            } else {
                Err(err(
                    first.col,
                    format!(
                        "{directive} expects {n} argument{}, got {}",
                        if n == 1 { "" } else { "s" },
                        args.len()
                    ),
                    None,
                ))
            }
        };
        let singleton = !matches!(directive, "INSTALL" | "FETCH" | "PROVISION" | "SETTING");
        if singleton {
            if let Some(prev) = seen.insert(directive, line) {
                return Err(err(
                    first.col,
                    format!("duplicate {directive} (first set on line {prev})"),
                    None,
                ));
            }
        }
        let bad = |t: &Token, m: String| err(t.col, m, None);
        match directive {
            "FROM" => {
                need(1)?;
                let r = ImageRef::parse(&args[0].text).map_err(|e| bad(&args[0], e.to_string()))?;
                if args[0].text.split(':').next() != Some("android") {
                    return Err(bad(
                        &args[0],
                        "FROM must reference an android:<api> base image".into(),
                    ));
                }
                from = Some((r, line));
            }
            "MEMORY" => {
                need(1)?;
                let mb = parse_size_mb(&args[0].text).map_err(|e| bad(&args[0], e.to_string()))?;
                if !(1024..=32768).contains(&mb) {
                    return Err(bad(
                        &args[0],
                        format!("MEMORY must be between 1GB and 32GB (got {mb}MB)"),
                    ));
                }
                d.memory_mb = mb;
            }
            "DISK" => {
                need(1)?;
                let mb = parse_size_mb(&args[0].text).map_err(|e| bad(&args[0], e.to_string()))?;
                if !(2048..=65536).contains(&mb) {
                    return Err(bad(
                        &args[0],
                        format!("DISK must be between 2GB and 64GB (got {mb}MB)"),
                    ));
                }
                d.disk_mb = mb;
            }
            "CPUS" => {
                need(1)?;
                let n: u32 = args[0]
                    .text
                    .parse()
                    .map_err(|_| bad(&args[0], "CPUS must be a number".into()))?;
                if !(1..=16).contains(&n) {
                    return Err(bad(&args[0], "CPUS must be between 1 and 16".into()));
                }
                d.cpus = n;
            }
            "DEVICE" => {
                need(1)?;
                let v = &args[0].text;
                if !v
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
                    || v.is_empty()
                {
                    return Err(bad(&args[0], format!("invalid device profile {v:?}")));
                }
                d.device = v.clone();
            }
            "RESOLUTION" => {
                need(1)?;
                let t = &args[0].text;
                let parsed = t
                    .split_once(['x', 'X'])
                    .and_then(|(w, h)| Some((w.parse::<u32>().ok()?, h.parse::<u32>().ok()?)));
                match parsed {
                    Some((w, h)) if (240..=4320).contains(&w) && (240..=4320).contains(&h) => {
                        d.resolution = (w, h)
                    }
                    _ => {
                        return Err(bad(
                            &args[0],
                            format!("invalid RESOLUTION {t:?}: expected WxH like 1080x2400"),
                        ));
                    }
                }
            }
            "HEADLESS" => {
                need(1)?;
                d.headless = match args[0].text.to_ascii_lowercase().as_str() {
                    "true" => true,
                    "false" => false,
                    _ => return Err(bad(&args[0], "HEADLESS must be true or false".into())),
                };
            }
            "GPU" => {
                need(1)?;
                let m = args[0].text.to_ascii_lowercase();
                if !GPU_MODES.contains(&m.as_str()) {
                    return Err(bad(
                        &args[0],
                        format!(
                            "unsupported GPU mode {:?}: use one of {}",
                            args[0].text,
                            GPU_MODES.join(", ")
                        ),
                    ));
                }
                d.gpu = Some(m);
            }
            "INSTALL" => {
                need(1)?;
                d.installs.push(args[0].text.clone());
            }
            "FETCH" => {
                need(1)?;
                d.fetches.push(args[0].text.clone());
            }
            "PROVISION" => {
                need(1)?;
                d.provisions.push(args[0].text.clone());
            }
            "SETTING" => {
                if !(2..=3).contains(&args.len()) {
                    return Err(err(
                        first.col,
                        "SETTING expects: SETTING [global|secure|system] <key> <value>".into(),
                        None,
                    ));
                }
                let (ns, rest) = if args.len() == 3 {
                    if !matches!(args[0].text.as_str(), "global" | "secure" | "system") {
                        return Err(bad(
                            &args[0],
                            "setting namespace must be global, secure or system".into(),
                        ));
                    }
                    (args[0].text.clone(), &args[1..])
                } else {
                    ("global".to_string(), args)
                };
                for t in rest {
                    if !safe_word(&t.text) {
                        return Err(bad(
                            t,
                            format!("unsupported characters in setting {:?}", t.text),
                        ));
                    }
                }
                d.settings.push(Setting {
                    namespace: ns,
                    key: rest[0].text.clone(),
                    value: rest[1].text.clone(),
                });
            }
            "SNAPSHOT" => {
                need(1)?;
                let n = &args[0].text;
                if !n
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                {
                    return Err(bad(&args[0], format!("invalid snapshot name {n:?}")));
                }
                d.snapshot = Some(n.clone());
            }
            "CMD" => {
                if args.len() != 2 || !args[0].text.eq_ignore_ascii_case("launch") {
                    return Err(err(
                        first.col,
                        "unsupported CMD: expected `CMD launch <package>`".into(),
                        None,
                    ));
                }
                if !valid_package(&args[1].text) {
                    return Err(bad(
                        &args[1],
                        format!("invalid package name {:?}", args[1].text),
                    ));
                }
                d.cmd = Some(Cmd::Launch(args[1].text.clone()));
            }
            _ => unreachable!(),
        }
    }

    match from {
        Some((r, _)) => {
            d.from = r;
            Ok(d)
        }
        None => Err(ParseError {
            file: file.to_string(),
            line: 1,
            col: 1,
            message: "missing FROM directive".into(),
            hint: Some("Add `FROM android:35` as the first line.".into()),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Result<Dorkerfile, ParseError> {
        parse("Dorkerfile", s)
    }

    #[test]
    fn full_example() {
        let d = p("FROM android:35\n\nMEMORY 2GB\ncpus 2\nDISK 4GB\nDEVICE pixel_7\nRESOLUTION 1080x2400\n\
                   HEADLESS true\nINSTALL ./app/app-debug.apk\nSETTING window_animation_scale 0\n\
                   SETTING secure show_ime_with_hard_keyboard 0 # trailing comment\nSNAPSHOT ready\n\
                   CMD launch com.example.application\n")
        .unwrap();
        assert_eq!(d.from.api, 35);
        assert_eq!((d.memory_mb, d.cpus, d.disk_mb), (2048, 2, 4096));
        assert_eq!(d.installs, vec!["./app/app-debug.apk"]);
        assert_eq!(d.settings.len(), 2);
        assert_eq!(d.settings[0].namespace, "global");
        assert_eq!(d.settings[1].namespace, "secure");
        assert_eq!(d.snapshot.as_deref(), Some("ready"));
        assert_eq!(d.cmd, Some(Cmd::Launch("com.example.application".into())));
    }

    /// Every ```dorkerfile block in the README and docs must parse, so the examples we show
    /// users can never drift out of sync with the parser.
    #[test]
    fn documented_dorkerfile_examples_parse() {
        let mut checked = 0;
        for (name, text) in [
            ("README.md", include_str!("../README.md")),
            ("docs/dorkerfile.md", include_str!("../docs/dorkerfile.md")),
        ] {
            let mut rest = text;
            while let Some(start) = rest.find("```dorkerfile\n") {
                let body = &rest[start + "```dorkerfile\n".len()..];
                let end = body.find("```").expect("unterminated code block");
                let block = &body[..end];
                parse(name, block)
                    .unwrap_or_else(|e| panic!("{name} example does not parse:\n{block}\n{e}"));
                checked += 1;
                rest = &body[end..];
            }
        }
        assert!(
            checked >= 2,
            "expected examples in the README and dorkerfile.md, found {checked}"
        );
    }

    #[test]
    fn fetch_is_repeatable_and_needs_one_path() {
        let d = p("FROM android:35\nFETCH ./a.sh\nFETCH ./b.sh\n").unwrap();
        assert_eq!(d.fetches, vec!["./a.sh", "./b.sh"]);
        assert!(p("FROM android:35\nFETCH\n").is_err());
        assert!(p("FROM android:35\nFETCH a b\n").is_err());
    }

    #[test]
    fn provision_is_repeatable_and_keeps_its_order() {
        let d =
            p("FROM android:35\nPROVISION ./a.sh\nINSTALL x.apk\nPROVISION \"b c.sh\"\n").unwrap();
        assert_eq!(d.provisions, vec!["./a.sh", "b c.sh"]);
        assert!(p("FROM android:35\nPROVISION\n").is_err());
        assert!(p("FROM android:35\nPROVISION a b\n").is_err());
    }

    #[test]
    fn gpu_directive() {
        let d = p("FROM android:35\nGPU host\n").unwrap();
        assert_eq!(d.gpu_mode(), "host");
        assert_eq!(
            p("FROM android:35\n").unwrap().gpu_mode(),
            if cfg!(target_os = "macos") {
                "host"
            } else {
                "swiftshader_indirect"
            }
        );
        assert_eq!(
            p("FROM android:35\nHEADLESS false\n").unwrap().gpu_mode(),
            "auto"
        );
        assert_eq!(
            p("FROM android:35\nGPU ANGLE_indirect\n")
                .unwrap()
                .gpu_mode(),
            "angle_indirect"
        );
        assert!(p("FROM android:35\nGPU turbo\n").is_err());
        assert!(p("FROM android:35\nGPU host\nGPU guest\n").is_err());
    }

    #[test]
    fn defaults_and_comments() {
        let d = p("# hi\nfrom android:35-google_apis\n").unwrap();
        assert_eq!(d.from.flavour.as_deref(), Some("google_apis"));
        assert_eq!((d.memory_mb, d.cpus, d.headless), (2048, 2, true));
    }

    #[test]
    fn quoted_paths() {
        let d = p("FROM android:35\nINSTALL \"my app/app debug.apk\"\n").unwrap();
        assert_eq!(d.installs, vec!["my app/app debug.apk"]);
        let e = p("FROM android:35\nINSTALL \"oops\n").unwrap_err();
        assert!(e.message.contains("unterminated"));
    }

    #[test]
    fn did_you_mean_with_position() {
        let e = p("FROM android:35\n\n\n\n\n\n\nMEM 2GB\n").unwrap_err();
        assert_eq!((e.line, e.col), (8, 1));
        assert_eq!(
            e.to_string(),
            "Dorkerfile:8:1\nerror: unsupported directive \"MEM\"\n\nDid you mean \"MEMORY\"?"
        );
        let e = p("FROM android:35\nINSTAL a.apk\n").unwrap_err();
        assert!(e.hint.unwrap().contains("INSTALL"));
    }

    #[test]
    fn requires_from_first() {
        assert!(
            p("MEMORY 2GB\n")
                .unwrap_err()
                .message
                .contains("before FROM")
        );
        assert!(
            p("# only comments\n")
                .unwrap_err()
                .message
                .contains("missing FROM")
        );
    }

    #[test]
    fn rejects_duplicates_but_allows_repeatable() {
        let e = p("FROM android:35\nMEMORY 2GB\nMEMORY 3GB\n").unwrap_err();
        assert_eq!(e.line, 3);
        assert!(e.message.contains("line 2"));
        assert!(
            p("FROM android:35\nINSTALL a.apk\nINSTALL b.apk\nSETTING a 1\nSETTING b 2\n").is_ok()
        );
        assert!(p("FROM android:35\nFROM android:34\n").is_err());
    }

    #[test]
    fn validates_values() {
        for bad in [
            "MEMORY 100MB",
            "MEMORY lots",
            "CPUS 0",
            "CPUS x",
            "DISK 1GB",
            "RESOLUTION 1080",
            "RESOLUTION 10x10",
            "HEADLESS maybe",
            "DEVICE Pixel 7",
            "SNAPSHOT bad/name",
            "SETTING only_key",
            "SETTING bogus k v",
            "SETTING k 'a;b'",
            "CMD run foo",
            "CMD launch notapackage",
            "MEMORY",
        ] {
            assert!(
                p(&format!("FROM android:35\n{bad}\n")).is_err(),
                "should reject {bad}"
            );
        }
        assert!(p("FROM myapp:latest\n").is_err());
        assert!(p("FROM android:abc\n").is_err());
    }
}
