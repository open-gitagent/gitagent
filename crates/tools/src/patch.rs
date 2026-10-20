//! The `apply_patch` tool: add, update, move or delete files with one patch.
//!
//! ```text
//! *** Begin Patch
//! *** Add File: path/new.txt
//! +first line of the new file
//! *** Update File: path/app.py
//! *** Move to: path/renamed.py
//! @@ def main():
//!      unchanged line
//! -    removed line
//! +    added line
//! *** Delete File: path/old.txt
//! *** End Patch
//! ```

use std::fs;
use std::path::{Path, PathBuf};

/// One file section of a patch.
enum Op {
    Add {
        path: String,
        content: String,
    },
    Delete(String),
    Update {
        path: String,
        dest: Option<String>,
        chunks: Vec<Chunk>,
    },
}

/// One `@@` section of an update.
#[derive(Default)]
struct Chunk {
    ctx: Option<String>,
    old: Vec<String>,
    new: Vec<String>,
    eof: bool,
}

impl Chunk {
    fn has_lines(&self) -> bool {
        !self.old.is_empty() || !self.new.is_empty()
    }
}

/// A file write planned while checking the patch; nothing touches the disk
/// until every section has been checked.
enum Write {
    Put(PathBuf, String),
    Remove(PathBuf),
}

/// Apply `patch` under `cwd`. Returns one line per changed file, or a message
/// for the model explaining why nothing was changed.
pub(crate) fn apply(patch: &str, cwd: &Path) -> Result<Vec<String>, String> {
    let mut writes: Vec<Write> = Vec::new();
    let mut done = Vec::new();
    for op in parse(patch)? {
        match op {
            Op::Add { path, content } => {
                writes.push(Write::Put(cwd.join(&path), content));
                done.push(format!("added {path}"));
            }
            Op::Delete(path) => {
                let full = cwd.join(&path);
                if !full.is_file() {
                    return Err(format!("cannot delete {path}: no such file"));
                }
                writes.push(Write::Remove(full));
                done.push(format!("deleted {path}"));
            }
            Op::Update { path, dest, chunks } => {
                let full = cwd.join(&path);
                // An earlier section of the same patch may already have
                // rewritten this file.
                let pending = writes.iter().rev().find_map(|w| match w {
                    Write::Put(p, text) if *p == full => Some(text.clone()),
                    _ => None,
                });
                let text = match pending {
                    Some(text) => text,
                    None => {
                        fs::read_to_string(&full).map_err(|e| format!("cannot read {path}: {e}"))?
                    }
                };
                let text = update(&text, &chunks).map_err(|e| format!("{path}: {e}"))?;
                match dest.filter(|d| *d != path) {
                    Some(dest) => {
                        writes.push(Write::Put(cwd.join(&dest), text));
                        writes.push(Write::Remove(full));
                        done.push(format!("updated {path} and moved it to {dest}"));
                    }
                    None => {
                        writes.push(Write::Put(full, text));
                        done.push(format!("updated {path}"));
                    }
                }
            }
        }
    }
    for write in writes {
        match write {
            Write::Put(full, text) => {
                if let Some(parent) = full.parent() {
                    fs::create_dir_all(parent)
                        .map_err(|e| format!("cannot write {}: {e}", full.display()))?;
                }
                fs::write(&full, text)
                    .map_err(|e| format!("cannot write {}: {e}", full.display()))?;
            }
            Write::Remove(full) => fs::remove_file(&full)
                .map_err(|e| format!("cannot remove {}: {e}", full.display()))?,
        }
    }
    Ok(done)
}

/// The lines of a patch body with a read position.
struct Cursor<'a> {
    lines: &'a [&'a str],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn peek(&self) -> Option<&'a str> {
        self.lines.get(self.at).copied()
    }

    fn next(&mut self) -> Option<&'a str> {
        let line = self.peek()?;
        self.at += 1;
        Some(line)
    }

    /// Patch line number of the body line at `index`, for error messages.
    fn line_no(&self, index: usize) -> usize {
        index + 2
    }
}

/// The path after a `*** <kind>:` header, if `line` is one.
fn header(line: &str, kind: &str) -> Option<String> {
    let rest = line
        .strip_prefix("*** ")?
        .strip_prefix(kind)?
        .strip_prefix(':')?;
    Some(rest.trim().to_owned())
}

fn is_file_header(line: &str) -> bool {
    ["Add File", "Update File", "Delete File"]
        .iter()
        .any(|kind| header(line, kind).is_some())
}

fn parse(patch: &str) -> Result<Vec<Op>, String> {
    let lines: Vec<&str> = patch.trim().lines().collect();
    if lines.first().map(|l| l.trim_end()) != Some("*** Begin Patch") {
        return Err("the patch must start with '*** Begin Patch'".into());
    }
    if lines.len() < 2 || lines.last().map(|l| l.trim_end()) != Some("*** End Patch") {
        return Err("the patch must end with '*** End Patch'".into());
    }
    let mut cur = Cursor {
        lines: &lines[1..lines.len() - 1],
        at: 0,
    };
    let mut ops = Vec::new();
    while let Some(line) = cur.next() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        if let Some(path) = header(line, "Add File") {
            ops.push(parse_add(path, &mut cur)?);
        } else if let Some(path) = header(line, "Delete File") {
            ops.push(Op::Delete(path));
        } else if let Some(path) = header(line, "Update File") {
            ops.push(parse_update(path, &mut cur)?);
        } else {
            return Err(format!(
                "line {}: expected '*** Add File: ', '*** Update File: ' or '*** Delete File: ', got {line:?}",
                cur.line_no(cur.at - 1)
            ));
        }
    }
    if ops.is_empty() {
        return Err("the patch names no files".into());
    }
    Ok(ops)
}

fn parse_add(path: String, cur: &mut Cursor) -> Result<Op, String> {
    let start = cur.at;
    let mut body = Vec::new();
    while let Some(line) = cur.peek() {
        if line.starts_with("*** ") {
            break;
        }
        cur.next();
        body.push(line);
    }
    // Blank lines separating this section from the next are not content.
    while body.last().is_some_and(|l| l.trim().is_empty()) {
        body.pop();
    }
    let mut content = String::new();
    for (i, line) in body.iter().enumerate() {
        let Some(text) = line.strip_prefix('+') else {
            return Err(format!(
                "line {}: every line of an added file must start with '+', got {line:?}",
                cur.line_no(start + i)
            ));
        };
        content.push_str(text);
        content.push('\n');
    }
    Ok(Op::Add { path, content })
}

fn parse_update(path: String, cur: &mut Cursor) -> Result<Op, String> {
    let dest = cur.peek().and_then(|l| header(l, "Move to"));
    if dest.is_some() {
        cur.next();
    }
    let mut chunks = Vec::new();
    let mut chunk = Chunk::default();
    while let Some(line) = cur.peek() {
        if is_file_header(line) {
            break;
        }
        cur.next();
        if let Some(ctx) = line.strip_prefix("@@") {
            if chunk.has_lines() {
                chunks.push(std::mem::take(&mut chunk));
            }
            if !ctx.trim().is_empty() {
                chunk.ctx = Some(ctx.trim().to_owned());
            }
        } else if line.trim_end() == "*** End of File" {
            chunk.eof = true;
            if chunk.has_lines() {
                chunks.push(std::mem::take(&mut chunk));
            }
        } else if let Some(text) = line.strip_prefix(' ') {
            chunk.old.push(text.to_owned());
            chunk.new.push(text.to_owned());
        } else if let Some(text) = line.strip_prefix('-') {
            chunk.old.push(text.to_owned());
        } else if let Some(text) = line.strip_prefix('+') {
            chunk.new.push(text.to_owned());
        } else if line.trim().is_empty() {
            chunk.old.push(String::new());
            chunk.new.push(String::new());
        } else {
            return Err(format!(
                "line {}: {line:?} is not a patch line; start it with ' ', '-', '+' or '@@'",
                cur.line_no(cur.at - 1)
            ));
        }
    }
    if chunk.has_lines() {
        chunks.push(chunk);
    }
    if chunks.is_empty() && dest.is_none() {
        return Err(format!("{path}: the update has no changes"));
    }
    Ok(Op::Update { path, dest, chunks })
}

/// Apply the chunks of one update to `text`.
fn update(text: &str, chunks: &[Chunk]) -> Result<String, String> {
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let mut pos = 0;
    for chunk in chunks {
        let mut after_ctx = None;
        if let Some(ctx) = &chunk.ctx {
            let at = find(&lines, std::slice::from_ref(ctx), pos, false)
                .ok_or_else(|| format!("could not find the context line \"{ctx}\""))?;
            pos = at + 1;
            after_ctx = Some(pos);
        }
        let (mut old, mut new) = (&chunk.old[..], &chunk.new[..]);
        if old.is_empty() {
            let at = match after_ctx {
                Some(at) if !chunk.eof => at,
                _ => lines.len(),
            };
            lines.splice(at..at, new.iter().cloned());
            pos = at + new.len();
            continue;
        }
        let mut at = find(&lines, old, pos, chunk.eof);
        // Models often end a chunk with a blank line that the file lacks.
        if at.is_none() && old.last().is_some_and(|l| l.trim().is_empty()) {
            old = &old[..old.len() - 1];
            if new.last().is_some_and(|l| l.trim().is_empty()) {
                new = &new[..new.len() - 1];
            }
            at = find(&lines, old, pos, chunk.eof);
        }
        let Some(at) = at else {
            let shown: Vec<&str> = old.iter().take(10).map(String::as_str).collect();
            return Err(format!("could not find these lines:\n{}", shown.join("\n")));
        };
        lines.splice(at..at + old.len(), new.iter().cloned());
        pos = at + new.len();
    }
    let mut out = lines.join("\n");
    if !out.is_empty() || text.ends_with('\n') {
        out.push('\n');
    }
    Ok(out)
}

/// Index of `pat` in `lines` at or after `pos`, trying stricter comparisons
/// before lenient ones. With `eof`, a match ending at the last line wins.
fn find(lines: &[String], pat: &[String], pos: usize, eof: bool) -> Option<usize> {
    if pat.is_empty() || pat.len() > lines.len() {
        return None;
    }
    let last = lines.len() - pat.len();
    for mode in 0..4 {
        let matches = |at: usize| {
            lines[at..at + pat.len()]
                .iter()
                .zip(pat)
                .all(|(l, p)| same(l, p, mode))
        };
        if eof && last >= pos && matches(last) {
            return Some(last);
        }
        if let Some(at) = (pos..=last).find(|&at| matches(at)) {
            return Some(at);
        }
    }
    None
}

fn same(a: &str, b: &str, mode: u8) -> bool {
    match mode {
        0 => a == b,
        1 => a.trim_end() == b.trim_end(),
        2 => a.trim() == b.trim(),
        _ => a.trim().chars().map(ascii).eq(b.trim().chars().map(ascii)),
    }
}

/// Typographic characters a model may emit in place of ASCII.
fn ascii(c: char) -> char {
    match c {
        '\u{2010}'..='\u{2015}' | '\u{2212}' => '-',
        '\u{2018}'..='\u{201B}' => '\'',
        '\u{201C}'..='\u{201F}' => '"',
        '\u{00A0}' | '\u{2002}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}' => ' ',
        c => c,
    }
}

#[cfg(test)]
mod extra_tests {
    use super::*;

    fn dir_with(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (p, c) in files {
            std::fs::write(dir.path().join(p), c).unwrap();
        }
        dir
    }

    fn read(dir: &tempfile::TempDir, p: &str) -> String {
        std::fs::read_to_string(dir.path().join(p)).unwrap()
    }

    #[test]
    fn reports_a_missing_context_line() {
        let dir = dir_with(&[("f.txt", "a\nb\n")]);
        let patch = "*** Begin Patch\n*** Update File: f.txt\n@@ nope\n-b\n+c\n*** End Patch";
        let err = apply(patch, dir.path()).unwrap_err();
        assert!(
            err.contains("could not find the context line \"nope\""),
            "{err}"
        );
        assert_eq!(read(&dir, "f.txt"), "a\nb\n");
    }

    #[test]
    fn rejects_an_update_without_changes() {
        let dir = dir_with(&[("f.txt", "a\n")]);
        let patch = "*** Begin Patch\n*** Update File: f.txt\n*** End Patch";
        let err = apply(patch, dir.path()).unwrap_err();
        assert!(err.contains("has no changes"), "{err}");
    }

    #[test]
    fn drops_a_trailing_blank_line_the_file_lacks() {
        let dir = dir_with(&[("f.txt", "a\nb\n")]);
        let patch = "*** Begin Patch\n*** Update File: f.txt\n a\n-b\n+c\n\n*** End Patch";
        apply(patch, dir.path()).unwrap();
        assert_eq!(read(&dir, "f.txt"), "a\nc\n");
    }

    #[test]
    fn moves_without_chunks_and_updates_twice() {
        let dir = dir_with(&[("f.txt", "a\n"), ("g.txt", "x\n")]);
        let patch = "*** Begin Patch\n*** Update File: f.txt\n*** Move to: h.txt\n\
                     *** Update File: g.txt\n-x\n+y\n*** Update File: g.txt\n-y\n+z\n*** End Patch";
        let done = apply(patch, dir.path()).unwrap();
        assert_eq!(done[0], "updated f.txt and moved it to h.txt");
        assert!(!dir.path().join("f.txt").exists());
        assert_eq!(read(&dir, "h.txt"), "a\n");
        assert_eq!(read(&dir, "g.txt"), "z\n");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir_with(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (p, c) in files {
            let full = dir.path().join(p);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, c).unwrap();
        }
        dir
    }

    fn read(dir: &tempfile::TempDir, p: &str) -> String {
        std::fs::read_to_string(dir.path().join(p)).unwrap()
    }

    #[test]
    fn adds_updates_moves_and_deletes() {
        let dir = dir_with(&[
            ("app.py", "def main():\n    print(\"hi\")\n\nmain()\n"),
            ("old.txt", "bye\n"),
            ("move.txt", "a\nb\n"),
        ]);
        let patch = "*** Begin Patch
*** Add File: new/hello.txt
+hello
+world
*** Update File: app.py
@@ def main():
-    print(\"hi\")
+    print(\"hello\")
*** Delete File: old.txt
*** Update File: move.txt
*** Move to: moved/m.txt
@@
 a
-b
+c
*** End Patch";
        let summary = apply(patch, dir.path()).unwrap();
        assert_eq!(summary.len(), 4);
        assert_eq!(read(&dir, "new/hello.txt"), "hello\nworld\n");
        assert_eq!(
            read(&dir, "app.py"),
            "def main():\n    print(\"hello\")\n\nmain()\n"
        );
        assert!(!dir.path().join("old.txt").exists());
        assert!(!dir.path().join("move.txt").exists());
        assert_eq!(read(&dir, "moved/m.txt"), "a\nc\n");
    }

    #[test]
    fn matches_with_whitespace_and_typographic_differences() {
        let dir = dir_with(&[("f.txt", "x = 1   \n  y – z\ndone\n")]);
        let patch = "*** Begin Patch
*** Update File: f.txt
-x = 1
- y - z
+x = 2
+y = z
*** End Patch";
        apply(patch, dir.path()).unwrap();
        assert_eq!(read(&dir, "f.txt"), "x = 2\ny = z\ndone\n");
    }

    #[test]
    fn several_chunks_apply_in_order() {
        let dir = dir_with(&[("f.txt", "a\nx\nb\nx\nc\n")]);
        let patch = "*** Begin Patch
*** Update File: f.txt
@@ b
-x
+second
@@
 a
-x
+first
*** End Patch";
        // The second chunk searches from after the first; `a` is before it.
        let err = apply(patch, dir.path()).unwrap_err();
        assert!(err.contains("could not find these lines"), "{err}");
        // Nothing was written when a chunk failed.
        assert_eq!(read(&dir, "f.txt"), "a\nx\nb\nx\nc\n");

        let patch = "*** Begin Patch
*** Update File: f.txt
 a
-x
+first
@@ b
-x
+second
*** End Patch";
        apply(patch, dir.path()).unwrap();
        assert_eq!(read(&dir, "f.txt"), "a\nfirst\nb\nsecond\nc\n");
    }

    #[test]
    fn end_of_file_and_pure_additions() {
        let dir = dir_with(&[("f.txt", "x\ny\nx\n")]);
        let patch = "*** Begin Patch
*** Update File: f.txt
-x
+last
*** End of File
*** End Patch";
        apply(patch, dir.path()).unwrap();
        assert_eq!(read(&dir, "f.txt"), "x\ny\nlast\n");
        let patch = "*** Begin Patch
*** Update File: f.txt
+appended
*** End Patch";
        apply(patch, dir.path()).unwrap();
        assert_eq!(read(&dir, "f.txt"), "x\ny\nlast\nappended\n");
    }

    #[test]
    fn reports_malformed_patches() {
        let dir = dir_with(&[]);
        for (patch, msg) in [
            ("hello", "must start with"),
            ("*** Begin Patch\n*** Add File: a\n+x", "must end with"),
            ("*** Begin Patch\n*** End Patch", "no files"),
            (
                "*** Begin Patch\n*** Add File: a\nx\n*** End Patch",
                "must start with '+'",
            ),
            (
                "*** Begin Patch\n*** Frob File: a\n*** End Patch",
                "expected '*** Add File",
            ),
            (
                "*** Begin Patch\n*** Delete File: none\n*** End Patch",
                "no such file",
            ),
        ] {
            let err = apply(patch, dir.path()).unwrap_err();
            assert!(err.contains(msg), "{patch:?}: {err}");
        }
    }
}
