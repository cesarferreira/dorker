//! Minimal verbose logging (`-v/--verbose` or `DORKER_VERBOSE=1`), written to stderr.
//!
//! dorker is a spawn-and-poll CLI, so a full tracing stack would be more machinery than
//! it needs. This prints what dorker decided and ran, with elapsed time, which is what is
//! needed to diagnose an emulator that will not start.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

static VERBOSE: AtomicBool = AtomicBool::new(false);
static START: OnceLock<Instant> = OnceLock::new();

pub fn set_verbose(on: bool) {
    START.get_or_init(Instant::now);
    VERBOSE.store(on, Ordering::Relaxed);
}

pub fn enabled() -> bool {
    VERBOSE.load(Ordering::Relaxed)
}

pub fn elapsed_secs() -> f64 {
    START.get().map_or(0.0, |s| s.elapsed().as_secs_f64())
}

/// `vlog!("starting {}", name)` prints `[dorker +1.23s] starting x` only in verbose mode.
macro_rules! vlog {
    ($($arg:tt)*) => {
        if $crate::log::enabled() {
            eprintln!("[dorker +{:.2}s] {}", $crate::log::elapsed_secs(), format_args!($($arg)*));
        }
    };
}
pub(crate) use vlog;

/// Render a command for logs, quoting arguments that contain spaces.
pub fn render_command(program: &std::path::Path, args: &[std::ffi::OsString]) -> String {
    let q = |s: &str| {
        if s.is_empty() || s.contains(char::is_whitespace) {
            format!("{s:?}")
        } else {
            s.to_string()
        }
    };
    let mut out = q(&program.to_string_lossy());
    for a in args {
        out.push(' ');
        out.push_str(&q(&a.to_string_lossy()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::path::Path;

    #[test]
    fn renders_and_quotes_commands() {
        let args: Vec<OsString> = vec!["-avd".into(), "dorker".into(), "a b".into(), "".into()];
        assert_eq!(
            render_command(Path::new("/sdk/emulator"), &args),
            "/sdk/emulator -avd dorker \"a b\" \"\""
        );
    }

    #[test]
    fn disabled_by_default_and_toggles() {
        set_verbose(false);
        assert!(!enabled());
        set_verbose(true);
        assert!(enabled());
        set_verbose(false);
    }
}
