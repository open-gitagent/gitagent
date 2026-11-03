use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use serde::Serialize;

/// Fired/consumed counters for every feature (BLUEPRINT §8).
///
/// Plain counters serialize as numbers, labelled ones (such as `tool_calls`)
/// as an object keyed by label.
#[derive(Debug, Default, Clone, Serialize, PartialEq, Eq)]
pub struct Counters {
    #[serde(flatten)]
    plain: BTreeMap<String, u64>,
    #[serde(flatten)]
    labelled: BTreeMap<String, BTreeMap<String, u64>>,
}

impl Counters {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn inc(&mut self, name: &str) {
        self.add(name, 1);
    }

    pub fn add(&mut self, name: &str, n: u64) {
        *self.plain.entry(name.to_string()).or_default() += n;
    }

    pub fn inc_labelled(&mut self, name: &str, label: &str) {
        *self
            .labelled
            .entry(name.to_string())
            .or_default()
            .entry(label.to_string())
            .or_default() += 1;
    }

    pub fn get(&self, name: &str) -> u64 {
        self.plain.get(name).copied().unwrap_or(0)
    }

    pub fn get_labelled(&self, name: &str, label: &str) -> u64 {
        self.labelled
            .get(name)
            .and_then(|m| m.get(label))
            .copied()
            .unwrap_or(0)
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_default()
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        crate::write_atomic(path, &serde_json::to_vec_pretty(self)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_and_serializes() {
        let mut c = Counters::new();
        c.inc("turns");
        c.inc("turns");
        c.add("input_tokens", 120);
        c.inc_labelled("tool_calls", "exec");
        c.inc_labelled("tool_calls", "exec");
        c.inc_labelled("tool_calls", "read_file");
        assert_eq!(c.get("turns"), 2);
        assert_eq!(c.get("missing"), 0);
        assert_eq!(c.get_labelled("tool_calls", "exec"), 2);
        assert_eq!(
            c.to_json(),
            serde_json::json!({
                "input_tokens": 120,
                "tool_calls": {"exec": 2, "read_file": 1},
                "turns": 2
            })
        );

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/counters.json");
        c.write(&path).unwrap();
        assert!(path.exists());
    }
}
