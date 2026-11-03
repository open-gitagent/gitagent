//! The `str_replace` tool: replace one exact occurrence of a text in a file.
//!
//! An exact match must be unique. When there is none, a whitespace-tolerant
//! match is tried: the same lines compared with surrounding whitespace
//! removed, and the replacement re-indented to the file's indentation. Those
//! fallback edits are counted separately.

use std::path::Path;

/// Result of an edit, for the tool output and counters.
pub(crate) struct Edited {
    pub message: String,
    pub fallback: bool,
}

pub(crate) fn edit(path: &Path, old: &str, new: &str, replace_all: bool) -> Result<Edited, String> {
    if old.is_empty() {
        return Err("`old_str` is empty; use write_file to create or replace a whole file".into());
    }
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;

    let matches: Vec<usize> = text.match_indices(old).map(|(i, _)| i).collect();
    let (updated, count, fallback) = match matches.len() {
        0 => {
            let (start, end, indent) = fuzzy_find(&text, old).ok_or_else(|| {
                format!(
                    "`old_str` was not found in {}. Read the file again and copy the exact text, including indentation.",
                    path.display()
                )
            })?;
            let replacement = reindent(new, &indent);
            let mut out = String::with_capacity(text.len());
            out.push_str(&text[..start]);
            out.push_str(&replacement);
            out.push_str(&text[end..]);
            (out, 1, true)
        }
        1 => (text.replacen(old, new, 1), 1, false),
        n if replace_all => (text.replace(old, new), n, false),
        n => {
            let lines: Vec<String> = matches
                .iter()
                .map(|&i| (text[..i].matches('\n').count() + 1).to_string())
                .collect();
            return Err(format!(
                "`old_str` appears {n} times in {} (lines {}). Include more surrounding text to make it unique, or set replace_all.",
                path.display(),
                lines.join(", ")
            ));
        }
    };
    std::fs::write(path, &updated).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    let at_line = updated
        .find(new)
        .map(|i| updated[..i].matches('\n').count() + 1);
    let message = match (count, at_line) {
        (1, Some(l)) => format!("Edited {} at line {l}.", path.display()),
        (n, _) => format!("Edited {} ({n} replacements).", path.display()),
    };
    Ok(Edited {
        message: if fallback {
            format!("{message} The text matched only after ignoring whitespace differences; check the result.")
        } else {
            message
        },
        fallback,
    })
}

/// Finds the lines of `old` in `text` comparing trimmed lines. Returns the
/// byte range of the matched lines and the indentation of the first one.
fn fuzzy_find(text: &str, old: &str) -> Option<(usize, usize, String)> {
    let want: Vec<&str> = old.trim_matches('\n').lines().map(str::trim).collect();
    if want.iter().all(|l| l.is_empty()) {
        return None;
    }
    let mut starts = Vec::new();
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        starts.push(offset);
        offset += line.len();
    }
    let lines: Vec<&str> = text.lines().collect();
    let mut found = None;
    for i in 0..lines.len().saturating_sub(want.len() - 1) {
        if (0..want.len()).all(|k| lines[i + k].trim() == want[k]) {
            if found.is_some() {
                return None; // ambiguous
            }
            found = Some(i);
        }
    }
    let i = found?;
    let last = i + want.len() - 1;
    let start = starts[i];
    let end = starts[last] + lines[last].len();
    let indent: String = lines[i].chars().take_while(|c| c.is_whitespace()).collect();
    Some((start, end, indent))
}

/// Shifts `new` so its first line has `indent`, keeping relative indentation.
fn reindent(new: &str, indent: &str) -> String {
    let lines: Vec<&str> = new.trim_matches('\n').lines().collect();
    let base = lines
        .first()
        .map(|l| l.len() - l.trim_start().len())
        .unwrap_or(0);
    lines
        .iter()
        .map(|l| {
            if l.trim().is_empty() {
                String::new()
            } else {
                let own = l.len() - l.trim_start().len();
                format!(
                    "{indent}{}{}",
                    " ".repeat(own.saturating_sub(base)),
                    l.trim_start()
                )
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(content: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.py");
        std::fs::write(&p, content).unwrap();
        (dir, p)
    }

    #[test]
    fn replaces_a_unique_exact_match() {
        let (_d, p) = file("a = 1\nb = 2\n");
        let e = edit(&p, "b = 2", "b = 3", false).unwrap();
        assert!(!e.fallback);
        assert!(e.message.ends_with("at line 2."), "{}", e.message);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "a = 1\nb = 3\n");
    }

    #[test]
    fn errors_on_missing_or_ambiguous_text() {
        let (_d, p) = file("x\ny\nx\n");
        let err = edit(&p, "x", "z", false).err().unwrap();
        assert!(
            err.contains("appears 2 times") && err.contains("lines 1, 3"),
            "{err}"
        );
        let err = edit(&p, "nothing", "z", false).err().unwrap();
        assert!(err.contains("was not found"), "{err}");
        assert!(edit(&p, "", "z", false).is_err());
        let e = edit(&p, "x", "z", true).unwrap();
        assert!(e.message.contains("2 replacements"));
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "z\ny\nz\n");
    }

    #[test]
    fn falls_back_to_whitespace_tolerant_matching() {
        let (_d, p) = file("def f():\n    if x:\n        return 1\n    return 2\n");
        // Indentation in old_str is off, and new_str uses its own indentation.
        let e = edit(&p, "if x:\n    return 1", "if y:\n    return 10", false).unwrap();
        assert!(e.fallback);
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "def f():\n    if y:\n        return 10\n    return 2\n"
        );
    }
}
