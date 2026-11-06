//! JUnit XML for `dorker test --image`: one `<testsuite>` per shard and one `<testcase>` per
//! result the runner reported. A problem that is not a test result (a shard that did not
//! start or finish, a test reported by two shards, a worker that could not be removed) is an
//! `<error>` test case named after the problem, so CI shows it. No passing test case is ever
//! made up.

use std::fmt::Write;

pub enum Outcome {
    Passed,
    Failure { message: String, detail: String },
    Error { message: String, detail: String },
    Skipped,
}

pub struct Case {
    pub class: String,
    pub name: String,
    pub secs: Option<f64>,
    pub outcome: Outcome,
}

pub struct Suite {
    pub name: String,
    pub hostname: Option<String>,
    pub secs: Option<f64>,
    pub properties: Vec<(String, String)>,
    pub cases: Vec<Case>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct Counts {
    pub tests: u32,
    pub failures: u32,
    pub errors: u32,
    pub skipped: u32,
}

impl std::ops::AddAssign for Counts {
    fn add_assign(&mut self, o: Counts) {
        self.tests += o.tests;
        self.failures += o.failures;
        self.errors += o.errors;
        self.skipped += o.skipped;
    }
}

fn suite_counts(s: &Suite) -> Counts {
    let mut c = Counts::default();
    for case in &s.cases {
        c.tests += 1;
        match case.outcome {
            Outcome::Passed => {}
            Outcome::Failure { .. } => c.failures += 1,
            Outcome::Error { .. } => c.errors += 1,
            Outcome::Skipped => c.skipped += 1,
        }
    }
    c
}

pub fn counts(suites: &[Suite]) -> Counts {
    let mut c = Counts::default();
    for s in suites {
        c += suite_counts(s);
    }
    c
}

/// The first line of a stack trace, short enough for a `message` attribute.
pub fn headline(stack: &str) -> String {
    let line = stack.lines().next().unwrap_or("").trim();
    match line.char_indices().nth(200) {
        Some((i, _)) => format!("{}...", &line[..i]),
        None => line.to_string(),
    }
}

/// Escape text for XML 1.0 content and attribute values. Characters XML cannot carry at all
/// (most control characters) become U+FFFD.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\t' | '\n' | '\r' => out.push(c),
            c if (c as u32) < 0x20 || c == '\u{FFFE}' || c == '\u{FFFF}' => out.push('\u{FFFD}'),
            c => out.push(c),
        }
    }
    out
}

fn time_attr(secs: Option<f64>) -> String {
    secs.map_or(String::new(), |t| format!(" time=\"{t:.3}\""))
}

pub fn render(name: &str, suites: &[Suite], secs: f64) -> String {
    let total = counts(suites);
    let mut x = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    let _ = writeln!(
        x,
        "<testsuites name=\"{}\" tests=\"{}\" failures=\"{}\" errors=\"{}\" skipped=\"{}\" time=\"{secs:.3}\">",
        escape(name),
        total.tests,
        total.failures,
        total.errors,
        total.skipped
    );
    for s in suites {
        let c = suite_counts(s);
        let _ = write!(
            x,
            "  <testsuite name=\"{}\" tests=\"{}\" failures=\"{}\" errors=\"{}\" skipped=\"{}\"{}",
            escape(&s.name),
            c.tests,
            c.failures,
            c.errors,
            c.skipped,
            time_attr(s.secs)
        );
        if let Some(h) = &s.hostname {
            let _ = write!(x, " hostname=\"{}\"", escape(h));
        }
        x.push_str(">\n");
        if !s.properties.is_empty() {
            x.push_str("    <properties>\n");
            for (k, v) in &s.properties {
                let _ = writeln!(
                    x,
                    "      <property name=\"{}\" value=\"{}\"/>",
                    escape(k),
                    escape(v)
                );
            }
            x.push_str("    </properties>\n");
        }
        for case in &s.cases {
            let _ = write!(
                x,
                "    <testcase classname=\"{}\" name=\"{}\"{}",
                escape(&case.class),
                escape(&case.name),
                time_attr(case.secs)
            );
            match &case.outcome {
                Outcome::Passed => x.push_str("/>\n"),
                Outcome::Skipped => x.push_str(">\n      <skipped/>\n    </testcase>\n"),
                Outcome::Failure { message, detail } | Outcome::Error { message, detail } => {
                    let tag = if matches!(case.outcome, Outcome::Failure { .. }) {
                        "failure"
                    } else {
                        "error"
                    };
                    let _ = write!(
                        x,
                        ">\n      <{tag} message=\"{}\">{}</{tag}>\n    </testcase>\n",
                        escape(message),
                        escape(detail)
                    );
                }
            }
        }
        x.push_str("  </testsuite>\n");
    }
    x.push_str("</testsuites>\n");
    x
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<Suite> {
        vec![
            Suite {
                name: "shard-0".into(),
                hostname: Some("emulator-5554".into()),
                secs: Some(1.5),
                properties: vec![("dorker.status".into(), "failed".into())],
                cases: vec![
                    Case {
                        class: "a.B".into(),
                        name: "ok".into(),
                        secs: Some(0.25),
                        outcome: Outcome::Passed,
                    },
                    Case {
                        class: "a.B<T>".into(),
                        name: "quotes \"&\" 'apostrophes'".into(),
                        secs: None,
                        outcome: Outcome::Failure {
                            message: "expected <1> but was <2>".into(),
                            detail: "java.lang.AssertionError: expected <1> & \u{1} done\n\tat a.B.x(B.java:1)\n]]>".into(),
                        },
                    },
                    Case {
                        class: "a.B".into(),
                        name: "skip".into(),
                        secs: None,
                        outcome: Outcome::Skipped,
                    },
                ],
            },
            Suite {
                name: "shard-1".into(),
                hostname: None,
                secs: None,
                properties: vec![],
                cases: vec![Case {
                    class: "dorker".into(),
                    name: "shard 1 timed_out".into(),
                    secs: None,
                    outcome: Outcome::Error {
                        message: "did not finish within 2s".into(),
                        detail: String::new(),
                    },
                }],
            },
        ]
    }

    #[test]
    fn junit_parses_and_round_trips_awkward_text() {
        let suites = sample();
        let xml = render("dorker <run>", &suites, 3.0);
        let doc = roxmltree::Document::parse(&xml).expect("valid XML");
        let root = doc.root_element();
        assert_eq!(root.tag_name().name(), "testsuites");
        assert_eq!(root.attribute("name"), Some("dorker <run>"));
        assert_eq!(root.attribute("tests"), Some("4"));
        assert_eq!(root.attribute("failures"), Some("1"));
        assert_eq!(root.attribute("errors"), Some("1"));
        assert_eq!(root.attribute("skipped"), Some("1"));

        let cases: Vec<_> = doc
            .descendants()
            .filter(|n| n.has_tag_name("testcase"))
            .collect();
        assert_eq!(cases.len(), 4);
        assert_eq!(cases[0].attribute("time"), Some("0.250"));
        assert_eq!(cases[1].attribute("classname"), Some("a.B<T>"));
        assert_eq!(
            cases[1].attribute("name"),
            Some("quotes \"&\" 'apostrophes'")
        );
        assert_eq!(cases[1].attribute("time"), None, "no invented timings");
        let failure = cases[1].first_element_child().unwrap();
        assert_eq!(failure.tag_name().name(), "failure");
        assert_eq!(
            failure.attribute("message"),
            Some("expected <1> but was <2>")
        );
        let text = failure.text().unwrap();
        assert!(text.contains("expected <1> & \u{FFFD} done"), "{text}");
        assert!(text.ends_with("]]>"), "{text}");
        assert!(cases[2].children().any(|n| n.has_tag_name("skipped")));
        assert_eq!(
            cases[3].first_element_child().unwrap().tag_name().name(),
            "error"
        );

        let shard0 = doc
            .descendants()
            .find(|n| n.has_tag_name("testsuite"))
            .unwrap();
        assert_eq!(shard0.attribute("hostname"), Some("emulator-5554"));
        assert_eq!(shard0.attribute("tests"), Some("3"));
        assert_eq!(
            counts(&suites),
            Counts {
                tests: 4,
                failures: 1,
                errors: 1,
                skipped: 1
            }
        );
    }

    #[test]
    fn an_empty_run_is_still_a_valid_document() {
        let xml = render("dorker", &[], 0.0);
        let doc = roxmltree::Document::parse(&xml).unwrap();
        assert_eq!(doc.root_element().attribute("tests"), Some("0"));
    }

    #[test]
    fn headlines_are_one_short_line() {
        assert_eq!(
            headline("java.lang.AssertionError: x\n\tat a"),
            "java.lang.AssertionError: x"
        );
        let long = "é".repeat(300);
        assert_eq!(headline(&long).chars().count(), 203);
        assert_eq!(headline(""), "");
    }
}
