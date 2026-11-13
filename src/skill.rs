//! `dorker skill install`: put the agent skill (`skill/SKILL.md`, embedded in the binary,
//! so it always matches this version) in `~/.agents/skills/dorker/`, where Codex, Copilot
//! CLI, Gemini CLI and other agents look for skills.

use crate::errors;
use crate::sdk::dirs_home;
use anyhow::{Context, Result, anyhow};
use std::path::PathBuf;

const SKILL: &str = include_str!("../skill/SKILL.md");

/// `~/.agents/skills/dorker`
fn target_dir() -> Result<PathBuf> {
    let home = dirs_home()
        .ok_or_else(|| anyhow!("cannot find your home directory (HOME is not set)"))
        .map_err(errors::with(errors::CONFIG))?;
    Ok(home.join(".agents").join("skills").join("dorker"))
}

pub fn install() -> Result<i32> {
    let dir = target_dir()?;
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let file = dir.join("SKILL.md");
    std::fs::write(&file, SKILL).with_context(|| format!("writing {}", file.display()))?;
    println!("Installed the dorker skill: {}", file.display());
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_skill_has_the_frontmatter_agents_require() {
        let rest = SKILL.strip_prefix("---\n").expect("frontmatter");
        let (front, _) = rest.split_once("\n---\n").expect("closing ---");
        assert!(front.lines().any(|l| l == "name: dorker"));
        assert!(
            front
                .lines()
                .any(|l| l.starts_with("description: Use when"))
        );
    }
}
