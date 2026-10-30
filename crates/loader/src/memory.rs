//! Persistent memory in the project: `memory/MEMORY.md` (the working memory,
//! at most `max_lines` lines, 200 by default) and further layers declared in
//! `memory/memory.yaml`. The working memory and layers marked `load: always`
//! are shown in the system prompt; the `memory` tool reads and appends.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

pub const DEFAULT_MAX_LINES: usize = 200;

/// One memory file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layer {
    pub name: String,
    /// Relative to the memory directory.
    pub path: PathBuf,
    pub max_lines: usize,
    /// Shown in the system prompt at the start of every session.
    pub load_always: bool,
}

/// Where memory lives and how it behaves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryCfg {
    /// The `memory/` directory.
    pub dir: PathBuf,
    /// The layer `save` writes to by default: the one named `working`, else
    /// the first, else `MEMORY.md`.
    pub working: Layer,
    pub layers: Vec<Layer>,
    /// Commit each save to git (when the directory is in a repository).
    pub commit: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct MemoryYaml {
    layers: Vec<LayerYaml>,
}

#[derive(Debug, Deserialize)]
struct LayerYaml {
    name: String,
    path: String,
    max_lines: Option<usize>,
    /// `always` shows the layer in the system prompt.
    load: Option<String>,
}

impl MemoryCfg {
    /// The memory set-up of a project directory.
    pub fn for_project(project: &Path, commit: bool) -> Result<MemoryCfg> {
        let dir = project.join("memory");
        let yaml_path = dir.join("memory.yaml");
        let declared: MemoryYaml = if yaml_path.is_file() {
            let text = std::fs::read_to_string(&yaml_path)
                .with_context(|| format!("reading {}", yaml_path.display()))?;
            serde_saphyr::from_str(&text)
                .with_context(|| format!("parsing {}", yaml_path.display()))?
        } else {
            MemoryYaml::default()
        };
        let mut layers: Vec<Layer> = declared
            .layers
            .into_iter()
            .filter(|l| !l.path.ends_with('/'))
            .map(|l| Layer {
                load_always: l.load.as_deref() == Some("always"),
                max_lines: l.max_lines.unwrap_or(DEFAULT_MAX_LINES),
                // Paths are relative to `memory/`; 1.x wrote them from the
                // project root.
                path: PathBuf::from(l.path.strip_prefix("memory/").unwrap_or(&l.path)),
                name: l.name,
            })
            .collect();
        let working = layers
            .iter()
            .find(|l| l.name == "working")
            .or(layers.first())
            .cloned()
            .unwrap_or_else(|| Layer {
                name: "working".into(),
                path: PathBuf::from("MEMORY.md"),
                max_lines: DEFAULT_MAX_LINES,
                load_always: true,
            });
        // The working memory is always loaded.
        match layers.iter_mut().find(|l| l.path == working.path) {
            Some(l) => l.load_always = true,
            None => layers.insert(0, working.clone()),
        }
        Ok(MemoryCfg {
            dir,
            working: Layer {
                load_always: true,
                ..working
            },
            layers,
            commit,
        })
    }

    pub fn layer(&self, name: &str) -> Option<&Layer> {
        self.layers.iter().find(|l| l.name == name)
    }

    /// The memory section of the system prompt: each layer loaded at session
    /// start that has content beyond a heading, or nothing.
    pub fn prompt_section(&self) -> String {
        let mut parts = Vec::new();
        for layer in self.layers.iter().filter(|l| l.load_always) {
            let Ok(text) = std::fs::read_to_string(self.dir.join(&layer.path)) else {
                continue;
            };
            let has_content = text
                .lines()
                .any(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'));
            if has_content {
                parts.push(format!(
                    "### {} (memory/{})\n{}",
                    layer.name,
                    layer.path.display(),
                    text.trim_end()
                ));
            }
        }
        if parts.is_empty() {
            return String::new();
        }
        format!(
            "# Memory\n\nWhat you remembered in earlier sessions. Use the `memory` tool to add to it.\n\n{}\n",
            parts.join("\n\n")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_memory_md_and_reads_declared_layers() {
        let dir = tempfile::tempdir().unwrap();
        let m = MemoryCfg::for_project(dir.path(), true).unwrap();
        assert_eq!(m.working.path, PathBuf::from("MEMORY.md"));
        assert_eq!(m.working.max_lines, 200);
        assert_eq!(m.layers.len(), 1);

        std::fs::create_dir_all(dir.path().join("memory")).unwrap();
        std::fs::write(
            dir.path().join("memory/memory.yaml"),
            "layers:\n  - {name: main, path: MEMORY.md, max_lines: 50}\n  - {name: decisions, path: memory/key-decisions.md, load: always}\n  - {name: log, path: daily-log/}\n",
        )
        .unwrap();
        let m = MemoryCfg::for_project(dir.path(), false).unwrap();
        assert_eq!(m.working.name, "main");
        assert_eq!(m.working.max_lines, 50);
        let names: Vec<&str> = m.layers.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, vec!["main", "decisions"]);
        assert_eq!(m.layers[1].path, PathBuf::from("key-decisions.md"));
        assert!(m.layers.iter().all(|l| l.load_always));
    }

    #[test]
    fn the_prompt_shows_layers_with_content_only() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("memory")).unwrap();
        let m = MemoryCfg::for_project(dir.path(), false).unwrap();
        assert_eq!(m.prompt_section(), "");
        std::fs::write(dir.path().join("memory/MEMORY.md"), "# Memory\n").unwrap();
        assert_eq!(m.prompt_section(), "");
        std::fs::write(
            dir.path().join("memory/MEMORY.md"),
            "# Memory\n- The user prefers short answers.\n",
        )
        .unwrap();
        let section = m.prompt_section();
        assert!(section.starts_with("# Memory\n"));
        assert!(section.contains(
            "### working (memory/MEMORY.md)\n# Memory\n- The user prefers short answers."
        ));
    }
}
