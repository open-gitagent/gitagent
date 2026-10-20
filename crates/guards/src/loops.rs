//! Loop detector (BLUEPRINT §5): the same call with the same result several
//! times in a row means the agent is stuck.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

pub const LOOP_MESSAGE: &str =
    "You have repeated the same action with the same result. Change approach.";

/// Lines from each end of the output that go into the comparison.
const EDGE_LINES: usize = 20;

pub struct LoopDetector {
    repeats: usize,
    recent: Vec<u64>,
}

impl LoopDetector {
    pub fn new(repeats: u32) -> Self {
        LoopDetector {
            repeats: (repeats as usize).max(2),
            recent: Vec::new(),
        }
    }

    /// Records a call; returns true when the last `repeats` calls were the
    /// same call with the same (normalized) output. The history is then
    /// cleared, so the warning repeats only if the loop continues.
    pub fn observe(&mut self, tool: &str, arguments_json: &str, output: &str) -> bool {
        let key = key(tool, arguments_json, output);
        self.recent.push(key);
        if self.recent.len() > self.repeats {
            self.recent.remove(0);
        }
        let looping = self.recent.len() == self.repeats && self.recent.iter().all(|&k| k == key);
        if looping {
            self.recent.clear();
        }
        looping
    }
}

fn key(tool: &str, arguments_json: &str, output: &str) -> u64 {
    let mut h = DefaultHasher::new();
    tool.hash(&mut h);
    normalize(arguments_json).hash(&mut h);
    let lines: Vec<&str> = output.lines().collect();
    let edges: Vec<&str> = if lines.len() <= 2 * EDGE_LINES {
        lines
    } else {
        lines[..EDGE_LINES]
            .iter()
            .chain(&lines[lines.len() - EDGE_LINES..])
            .copied()
            .collect()
    };
    for line in edges {
        normalize(line).hash(&mut h);
    }
    h.finish()
}

/// Digits and surrounding whitespace vary between otherwise identical runs
/// (timestamps, durations, PIDs).
fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_digits = false;
    for c in s.trim().chars() {
        if c.is_ascii_digit() {
            if !in_digits {
                out.push('#');
            }
            in_digits = true;
        } else {
            out.push(c);
            in_digits = false;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_identical_calls_trigger_once() {
        let mut d = LoopDetector::new(3);
        let args = r#"{"cmd":"make"}"#;
        assert!(!d.observe("exec", args, "error at 12:00:01\n[exit 2 · 1.2s]"));
        assert!(!d.observe("exec", args, "error at 12:00:07\n[exit 2 · 1.3s]"));
        assert!(d.observe("exec", args, "error at 12:00:15\n[exit 2 · 1.1s]"));
        // The count restarts after a warning.
        assert!(!d.observe("exec", args, "error at 12:01:00\n[exit 2 · 1.1s]"));
    }

    #[test]
    fn different_output_or_arguments_break_the_run() {
        let mut d = LoopDetector::new(3);
        assert!(!d.observe("exec", "a", "x"));
        assert!(!d.observe("exec", "a", "y"));
        assert!(!d.observe("exec", "a", "y"));
        assert!(d.observe("exec", "a", "y"));
        let mut d = LoopDetector::new(3);
        assert!(!d.observe("exec", "a", "x"));
        assert!(!d.observe("exec", "b", "x"));
        assert!(!d.observe("exec", "a", "x"));
    }
}
