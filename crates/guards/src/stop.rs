//! Evidence-based stop check (BLUEPRINT §5): `task_complete` is accepted only
//! if something was verified after the last change. It never rewrites the
//! agent's work; it only accepts or rejects, at most `max_rejections` times.

use anyhow::{Context, Result};
use regex::Regex;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopVerdict {
    Accept,
    /// `last_edit` describes the change that was not verified.
    Reject {
        last_edit: String,
    },
}

pub struct StopCheck {
    edit: Vec<Regex>,
    verify: Vec<Regex>,
    max_rejections: u32,
    /// Position of the last change and a short description of it.
    last_edit: Option<(u64, String)>,
    last_verify: Option<u64>,
    rejections: u32,
}

impl StopCheck {
    pub fn new(
        edit_patterns: &[String],
        verify_patterns: &[String],
        max_rejections: u32,
    ) -> Result<Self> {
        let compile = |patterns: &[String]| -> Result<Vec<Regex>> {
            patterns
                .iter()
                .map(|p| Regex::new(p).with_context(|| format!("invalid stop pattern {p:?}")))
                .collect()
        };
        Ok(StopCheck {
            edit: compile(edit_patterns)?,
            verify: compile(verify_patterns)?,
            max_rejections,
            last_edit: None,
            last_verify: None,
            rejections: 0,
        })
    }

    /// Records a change made by a tool (e.g. `write_file`). `seq` orders
    /// events; it must grow with every tool call.
    pub fn record_edit(&mut self, seq: u64, what: &str) {
        self.last_edit = Some((seq, what.to_string()));
    }

    /// Classifies a shell command. A command can both change and verify
    /// (`sed -i ... && pytest`); it then counts as verified. Verification only
    /// counts when the command finished.
    pub fn observe_command(&mut self, seq: u64, cmd: &str, finished: bool) {
        let without_harmless = strip_harmless_redirects(cmd);
        if self.edit.iter().any(|r| r.is_match(&without_harmless)) {
            self.record_edit(seq, &first_line(cmd));
        }
        if finished && self.verify.iter().any(|r| r.is_match(cmd)) {
            self.last_verify = Some(seq);
        }
    }

    pub fn on_complete(&mut self) -> StopVerdict {
        let Some((edit_seq, what)) = &self.last_edit else {
            return StopVerdict::Accept;
        };
        let verified = self.last_verify.is_some_and(|v| v >= *edit_seq);
        if verified || self.rejections >= self.max_rejections {
            return StopVerdict::Accept;
        }
        self.rejections += 1;
        StopVerdict::Reject {
            last_edit: what.clone(),
        }
    }

    pub fn rejections(&self) -> u32 {
        self.rejections
    }
}

/// Removes redirections that do not write files (`2>&1`, `> /dev/null`), so
/// the redirect edit pattern does not match them.
fn strip_harmless_redirects(cmd: &str) -> String {
    // Built once per call; the commands are short.
    let harmless =
        Regex::new(r"[0-9]*>{1,2}\s*&\s*[0-9-]+|&?[0-9]*>{1,2}\s*/dev/(null|stdout|stderr)")
            .expect("valid regex");
    harmless.replace_all(cmd, " ").into_owned()
}

fn first_line(cmd: &str) -> String {
    let line = cmd
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    if line.chars().count() > 120 {
        format!("{}…", line.chars().take(120).collect::<String>())
    } else {
        line.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(list: &[&str]) -> Vec<String> {
        list.iter().map(|p| p.to_string()).collect()
    }

    fn check() -> StopCheck {
        StopCheck::new(
            &s(&[r"\bsed\s+(-\w+\s+)*-i", r">{1,2}\s*[^\s&|>]", r"\btee\b"]),
            &s(&[r"\bpytest\b", r"\bcurl\s", r"\bpython3?\s+(-c|\S+\.py)"]),
            2,
        )
        .unwrap()
    }

    #[test]
    fn edit_then_complete_is_rejected() {
        let mut c = check();
        c.observe_command(1, "sed -i 's/a/b/' app.py", true);
        assert_eq!(
            c.on_complete(),
            StopVerdict::Reject {
                last_edit: "sed -i 's/a/b/' app.py".into()
            }
        );
    }

    #[test]
    fn edit_test_complete_is_accepted() {
        let mut c = check();
        c.record_edit(1, "write_file app.py");
        c.observe_command(2, "pytest -q", true);
        assert_eq!(c.on_complete(), StopVerdict::Accept);
    }

    #[test]
    fn third_attempt_is_accepted_after_two_rejections() {
        let mut c = check();
        c.observe_command(1, "echo x > out.txt", true);
        assert!(matches!(c.on_complete(), StopVerdict::Reject { .. }));
        assert!(matches!(c.on_complete(), StopVerdict::Reject { .. }));
        assert_eq!(c.on_complete(), StopVerdict::Accept);
        assert_eq!(c.rejections(), 2);
    }

    #[test]
    fn no_edits_means_nothing_to_verify() {
        let mut c = check();
        c.observe_command(1, "ls -la; cat README", true);
        assert_eq!(c.on_complete(), StopVerdict::Accept);
    }

    #[test]
    fn verification_must_come_after_the_last_edit_and_finish() {
        let mut c = check();
        c.observe_command(1, "pytest", true);
        c.observe_command(2, "tee out.log < in", true);
        assert!(matches!(c.on_complete(), StopVerdict::Reject { .. }));
        // A test still running when its output was returned does not count.
        c.observe_command(3, "pytest", false);
        assert!(matches!(c.on_complete(), StopVerdict::Reject { .. }));
    }

    #[test]
    fn a_command_that_edits_and_verifies_counts_as_verified() {
        let mut c = check();
        c.observe_command(1, "sed -i 's/x/y/' f.py && python3 f.py", true);
        assert_eq!(c.on_complete(), StopVerdict::Accept);
    }

    #[test]
    fn harmless_redirects_are_not_edits() {
        let mut c = check();
        c.observe_command(
            1,
            "make 2>&1 | tail; grep x f > /dev/null; ls &>/dev/null",
            true,
        );
        assert_eq!(c.on_complete(), StopVerdict::Accept);
    }

    #[test]
    fn invalid_patterns_are_reported() {
        assert!(StopCheck::new(&s(&["("]), &[], 2).is_err());
    }
}
