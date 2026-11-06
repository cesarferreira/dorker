//! Embeds the `templates/` folder into the binary, so `dorker init --template` works from a
//! plain `cargo install` with no repository around. Adding a template is just adding a folder.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map(|rd| rd.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    entries.sort();
    for p in entries {
        if p.file_name().is_some_and(|n| n == ".DS_Store") {
            continue;
        }
        if p.is_dir() {
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

fn main() {
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("templates");
    println!("cargo:rerun-if-changed=templates");
    println!("cargo:rerun-if-changed=build.rs");
    let mut files = Vec::new();
    walk(&root, &mut files);

    let mut src = String::from(
        "/// One file of a template, embedded at build time.\n\
         pub struct EmbeddedFile {\n    pub template: &'static str,\n    /// Path inside the template, with `/` separators.\n    pub path: &'static str,\n    pub data: &'static [u8],\n    pub executable: bool,\n}\n\n\
         pub static EMBEDDED: &[EmbeddedFile] = &[\n",
    );
    for f in files {
        let rel = f.strip_prefix(&root).unwrap();
        let mut parts = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned());
        let (Some(template), Some(first)) = (parts.next(), parts.next()) else {
            continue; // a loose file directly under templates/
        };
        let path = std::iter::once(first)
            .chain(parts)
            .collect::<Vec<_>>()
            .join("/");
        #[cfg(unix)]
        let executable = {
            use std::os::unix::fs::PermissionsExt;
            std::fs::metadata(&f)
                .map(|m| m.permissions().mode() & 0o111 != 0)
                .unwrap_or(false)
        };
        #[cfg(not(unix))]
        let executable = false;
        let _ = writeln!(
            src,
            "    EmbeddedFile {{ template: {template:?}, path: {path:?}, data: include_bytes!({:?}), executable: {executable} }},",
            f.to_string_lossy()
        );
    }
    src.push_str("];\n");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("templates.rs");
    std::fs::write(out, src).unwrap();
}
