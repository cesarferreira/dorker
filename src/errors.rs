//! Stable process exit codes (documented in the README).

pub const CONFIG: i32 = 2; // invalid configuration or arguments
pub const DEPENDENCY: i32 = 3; // missing SDK, emulator, adb or system image
pub const BUILD: i32 = 4; // image build failure
pub const STARTUP: i32 = 5; // instance failed to start or boot
pub const ADB: i32 = 6; // adb operation failed
pub const TEST: i32 = 7; // instrumentation tests failed
pub const CONFLICT: i32 = 8; // name or port already in use

/// An error carrying a stable exit code. The innermost code wins.
#[derive(Debug)]
pub struct Coded {
    pub code: i32,
    pub inner: anyhow::Error,
}

impl std::fmt::Display for Coded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#}", self.inner)
    }
}

impl std::error::Error for Coded {}

/// `.map_err(errors::with(errors::CONFIG))`
pub fn with(code: i32) -> impl Fn(anyhow::Error) -> anyhow::Error + Copy {
    move |e| {
        if e.downcast_ref::<Coded>().is_some() {
            e
        } else {
            anyhow::Error::new(Coded { code, inner: e })
        }
    }
}

pub fn code_of(e: &anyhow::Error) -> i32 {
    e.downcast_ref::<Coded>().map_or(1, |c| c.code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_survive_and_innermost_wins() {
        let e = with(CONFIG)(anyhow::anyhow!("bad"));
        assert_eq!(code_of(&e), CONFIG);
        let e = with(BUILD)(e);
        assert_eq!(code_of(&e), CONFIG, "outer wrap must not override");
        assert_eq!(code_of(&anyhow::anyhow!("plain")), 1);
        assert_eq!(e.to_string(), "bad");
    }

    #[test]
    fn context_chain_is_preserved_in_display() {
        let e = with(DEPENDENCY)(anyhow::anyhow!("inner").context("outer"));
        assert_eq!(e.to_string(), "outer: inner");
    }
}
