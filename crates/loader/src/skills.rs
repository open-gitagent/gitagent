//! Skills: `skills/<name>/SKILL.md` files in a config directory. Their names
//! and descriptions go in the system prompt; the `skill` tool loads the full
//! text when the agent needs it.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
}

/// Skills from all config directories; for the same name the first directory
/// wins. Sorted by name, so the prompt index is stable.
pub fn discover(config_dirs: &[PathBuf]) -> Vec<Skill> {
    let mut skills: Vec<Skill> = Vec::new();
    for dir in config_dirs {
        let Ok(entries) = std::fs::read_dir(dir.join("skills")) else {
            continue;
        };
        let mut found: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path().join("SKILL.md"))
            .filter(|p| p.is_file())
            .collect();
        found.sort();
        for path in found {
            if let Some(skill) = read(&path) {
                if !skills.iter().any(|s| s.name == skill.name) {
                    skills.push(skill);
                }
            }
        }
    }
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

fn read(path: &Path) -> Option<Skill> {
    let text = std::fs::read_to_string(path).ok()?;
    let folder = path.parent()?.file_name()?.to_string_lossy().to_string();
    let (front, body) = split_frontmatter(&text);
    let field = |key: &str| {
        front.lines().find_map(|l| {
            l.strip_prefix(key)
                .and_then(|rest| rest.trim_start().strip_prefix(':'))
                .map(|v| v.trim().trim_matches('"').to_string())
        })
    };
    let name = field("name").filter(|n| !n.is_empty()).unwrap_or(folder);
    let description = field("description")
        .filter(|d| !d.is_empty())
        .or_else(|| {
            body.lines()
                .map(|l| l.trim().trim_start_matches('#').trim())
                .find(|l| !l.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_default();
    Some(Skill {
        name,
        description: description.chars().take(300).collect(),
        path: path.to_path_buf(),
    })
}

/// Splits a leading `---` ... `---` block from the body.
pub fn split_frontmatter(text: &str) -> (&str, &str) {
    let Some(rest) = text.strip_prefix("---") else {
        return ("", text);
    };
    match rest.find("\n---") {
        Some(end) => {
            let body = &rest[end + 4..];
            (&rest[..end], body.strip_prefix('\n').unwrap_or(body))
        }
        None => ("", text),
    }
}

/// The section added to the system prompt.
pub fn index(skills: &[Skill]) -> String {
    let mut out = String::from(
        "# Skills\n\nLoad a skill with the `skill` tool when it fits the task; it gives detailed instructions.\n",
    );
    for s in skills {
        out.push_str(&format!("- {}: {}\n", s.name, s.description));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_skills_with_and_without_frontmatter() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let write = |root: &Path, name: &str, text: &str| {
            std::fs::create_dir_all(root.join("skills").join(name)).unwrap();
            std::fs::write(root.join("skills").join(name).join("SKILL.md"), text).unwrap();
        };
        write(
            a.path(),
            "pdf",
            "---\nname: pdf\ndescription: Read and fill PDF forms.\n---\n# PDF\nUse pypdf.\n",
        );
        write(a.path(), "zeta", "# Zeta tricks\nBody.\n");
        write(
            b.path(),
            "pdf",
            "---\nname: pdf\ndescription: shadowed\n---\n",
        );
        write(
            b.path(),
            "alpha",
            "---\ndescription: \"Alpha things\"\n---\nx\n",
        );
        let skills = discover(&[a.path().to_path_buf(), b.path().to_path_buf()]);
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["alpha", "pdf", "zeta"]);
        assert_eq!(skills[0].description, "Alpha things");
        assert_eq!(skills[1].description, "Read and fill PDF forms.");
        assert_eq!(skills[2].description, "Zeta tricks");
        assert!(index(&skills).contains("- pdf: Read and fill PDF forms.\n"));
        assert_eq!(split_frontmatter("---\na: 1\n---\nbody").1, "body");
    }
}
