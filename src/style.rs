//! Minimal ANSI styling for human-readable output (no dependency).
//!
//! Colour is on only when stdout is a terminal, and follows the usual conventions:
//! `NO_COLOR` (any non-empty value) turns it off and wins over everything else,
//! `CLICOLOR_FORCE` (non-empty, not `0`) turns it on even when piped, `CLICOLOR=0` and
//! `TERM=dumb` turn it off. `--json` output is never styled.

use std::io::IsTerminal;

#[derive(Clone, Copy, Debug)]
pub struct Style {
    pub on: bool,
}

/// Decide whether to colour, from the environment and whether stdout is a terminal.
/// Pure (the environment is passed in) so it can be unit-tested.
pub fn color_enabled(is_tty: bool, env: &dyn Fn(&str) -> Option<String>) -> bool {
    let set = |k: &str| env(k).is_some_and(|v| !v.is_empty());
    if set("NO_COLOR") {
        return false;
    }
    if env("CLICOLOR_FORCE").is_some_and(|v| !v.is_empty() && v != "0") {
        return true;
    }
    if env("CLICOLOR").is_some_and(|v| v == "0") || env("TERM").is_some_and(|v| v == "dumb") {
        return false;
    }
    is_tty
}

impl Style {
    pub fn stdout() -> Self {
        let on = color_enabled(std::io::stdout().is_terminal(), &|k| std::env::var(k).ok());
        Style { on }
    }

    fn paint(&self, code: &str, s: &str) -> String {
        if self.on {
            format!("\x1b[{code}m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    pub fn green(&self, s: &str) -> String {
        self.paint("32", s)
    }
    pub fn yellow(&self, s: &str) -> String {
        self.paint("33", s)
    }
    pub fn red_bold(&self, s: &str) -> String {
        self.paint("1;31", s)
    }
    pub fn green_bold(&self, s: &str) -> String {
        self.paint("1;32", s)
    }
    pub fn cyan(&self, s: &str) -> String {
        self.paint("36", s)
    }
    pub fn bold(&self, s: &str) -> String {
        self.paint("1", s)
    }
    pub fn dim(&self, s: &str) -> String {
        self.paint("2", s)
    }
    /// Colour an instance status (`READY` green, failures red, idle dim, in-between yellow).
    /// `text` may be padded; `status` picks the colour.
    pub fn status(&self, status: &str, text: &str) -> String {
        match status {
            "READY" => self.green_bold(text),
            "FAILED" | "CRASHED" => self.red_bold(text),
            "STOPPED" | "CREATED" => self.dim(text),
            _ => self.yellow(text),
        }
    }
}

/// Remove ANSI escape sequences (used by tests to compare styled output with plain output).
#[cfg(test)]
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for n in chars.by_ref() {
                if n.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let m: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| m.get(k).cloned()
    }

    #[test]
    fn follows_the_terminal_by_default() {
        assert!(color_enabled(true, &env(&[])));
        assert!(!color_enabled(false, &env(&[])));
    }

    #[test]
    fn no_color_wins_over_everything() {
        assert!(!color_enabled(true, &env(&[("NO_COLOR", "1")])));
        assert!(!color_enabled(
            true,
            &env(&[("NO_COLOR", "1"), ("CLICOLOR_FORCE", "1")])
        ));
        // An empty NO_COLOR is ignored, per the convention.
        assert!(color_enabled(true, &env(&[("NO_COLOR", "")])));
    }

    #[test]
    fn force_and_opt_out_variables() {
        assert!(
            color_enabled(false, &env(&[("CLICOLOR_FORCE", "1")])),
            "forced on even when piped"
        );
        assert!(
            !color_enabled(false, &env(&[("CLICOLOR_FORCE", "0")])),
            "0 does not force"
        );
        assert!(!color_enabled(true, &env(&[("CLICOLOR", "0")])));
        assert!(!color_enabled(true, &env(&[("TERM", "dumb")])));
    }

    #[test]
    fn painting_is_a_no_op_when_off_and_reversible_when_on() {
        let off = Style { on: false };
        let on = Style { on: true };
        assert_eq!(off.red_bold("x"), "x");
        assert_eq!(on.red_bold("x"), "\x1b[1;31mx\x1b[0m");
        assert_eq!(strip_ansi(&on.green("ok")), "ok");
        assert_eq!(strip_ansi(&on.dim(&on.cyan("nested"))), "nested");
    }
}
