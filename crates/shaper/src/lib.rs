//! Tool output shaping (BLUEPRINT §5): the full output always goes to a spill
//! file first; the model sees a cleaned, collapsed and capped version that
//! says where the full output is.

use std::path::{Path, PathBuf};

/// Shaping limits, normally taken from the profile's `shape` section.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShapeCfg {
    /// Approximate token budget for one result.
    pub cap_tokens: u32,
    /// Share of the budget kept from the start; the rest comes from the end.
    pub head_ratio: f32,
    /// Collapse runs of at least this many similar lines; 0 turns collapsing off.
    pub collapse_min_run: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shaped {
    pub text: String,
    /// Where the full raw output was saved (absent if writing it failed).
    pub spill_path: Option<PathBuf>,
    /// Lines in the cleaned output, before collapsing and capping.
    pub total_lines: usize,
    /// Lines the model does not see (collapsed or cut).
    pub hidden_lines: usize,
    /// Runs of similar lines that were collapsed.
    pub collapsed_runs: usize,
    pub binary: bool,
}

/// Shapes successive tool outputs, numbering their spill files.
pub struct Shaper {
    pub cfg: ShapeCfg,
    pub spill_dir: PathBuf,
    next: u64,
}

impl Shaper {
    pub fn new(cfg: ShapeCfg, spill_dir: impl Into<PathBuf>) -> Self {
        Shaper {
            cfg,
            spill_dir: spill_dir.into(),
            next: 1,
        }
    }

    pub fn shape(&mut self, raw: &[u8]) -> Shaped {
        let seq = self.next;
        self.next += 1;
        shape(raw, &self.cfg, &self.spill_dir, seq)
    }

    /// Whether `text` (a command or a path) refers to a spill file.
    pub fn mentions_spill(&self, text: &str) -> bool {
        text.contains(&*self.spill_dir.to_string_lossy())
    }
}

/// Rough token estimate: about four bytes of text per token.
pub fn estimate_tokens(text: &str) -> usize {
    text.len().div_ceil(4)
}

/// Shapes `raw` for the model, saving it to `{spill_dir}/{seq}.log` first.
pub fn shape(raw: &[u8], cfg: &ShapeCfg, spill_dir: &Path, seq: u64) -> Shaped {
    let spill_path = spill(raw, spill_dir, seq);
    let see_log = |what: &str| match &spill_path {
        Some(p) => format!("{what} — full output: {}", p.display()),
        None => what.to_string(),
    };

    if is_binary(raw) {
        return Shaped {
            text: format!(
                "[{}]",
                see_log(&format!("binary output, {} bytes", raw.len()))
            ),
            spill_path,
            total_lines: 0,
            hidden_lines: 0,
            collapsed_runs: 0,
            binary: true,
        };
    }

    let cleaned = strip_ansi(&String::from_utf8_lossy(raw));
    let lines: Vec<&str> = cleaned.lines().map(last_frame).collect();
    let total_lines = lines.len();

    let (lines, collapsed_hidden, collapsed_runs) = collapse(lines, cfg.collapse_min_run as usize);
    let (mut text, cut_lines) = cap(&lines, cfg, &see_log);
    if raw.ends_with(b"\n") && !text.is_empty() {
        text.push('\n');
    }
    Shaped {
        text,
        spill_path,
        total_lines,
        hidden_lines: collapsed_hidden + cut_lines,
        collapsed_runs,
        binary: false,
    }
}

fn spill(raw: &[u8], dir: &Path, seq: u64) -> Option<PathBuf> {
    let path = dir.join(format!("{seq}.log"));
    std::fs::create_dir_all(dir).ok()?;
    std::fs::write(&path, raw).ok()?;
    Some(path)
}

/// More than 10% control bytes (other than whitespace and escapes) or any NUL.
fn is_binary(raw: &[u8]) -> bool {
    if raw.is_empty() {
        return false;
    }
    let sample = &raw[..raw.len().min(64 * 1024)];
    if sample.contains(&0) {
        return true;
    }
    let control = sample
        .iter()
        .filter(|&&b| (b < 0x20 && !matches!(b, b'\n' | b'\r' | b'\t' | 0x1b | 0x08)) || b == 0x7f)
        .count();
    let invalid = String::from_utf8_lossy(sample)
        .chars()
        .filter(|&c| c == char::REPLACEMENT_CHARACTER)
        .count();
    (control + invalid) * 10 > sample.len()
}

/// Removes ANSI escape sequences (CSI, OSC and two-byte escapes) and applies
/// backspaces.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => match chars.next() {
                Some('[') => {
                    // CSI: parameters, then one final byte in @..~
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    // OSC: ends with BEL or ESC \
                    while let Some(c) = chars.next() {
                        if c == '\x07' {
                            break;
                        }
                        if c == '\x1b' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                // Character-set selection such as ESC ( B takes one more byte.
                Some('(' | ')' | '*' | '+' | '-' | '.' | '/' | '#' | '%') => {
                    chars.next();
                }
                _ => {}
            },
            '\x08' => {
                out.pop();
            }
            c if c.is_control() && !matches!(c, '\n' | '\r' | '\t') => {}
            c => out.push(c),
        }
    }
    out
}

/// A line redrawn with carriage returns (progress bars) keeps its last frame.
fn last_frame(line: &str) -> &str {
    let line = line.strip_suffix('\r').unwrap_or(line);
    match line.rfind('\r') {
        Some(i) => &line[i + 1..],
        None => line,
    }
}

/// Each run of digits becomes one '#', so "step 9/10" and "step 10/10" look alike.
fn normalized(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut in_digits = false;
    for c in line.chars() {
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

/// Replaces each run of `min_run` or more similar lines with its first line,
/// a note and its last line. Returns the lines, how many were hidden and how
/// many runs were collapsed.
fn collapse(lines: Vec<&str>, min_run: usize) -> (Vec<String>, usize, usize) {
    if min_run < 3 {
        return (lines.into_iter().map(str::to_string).collect(), 0, 0);
    }
    let mut out = Vec::with_capacity(lines.len());
    let mut hidden = 0;
    let mut runs = 0;
    let mut i = 0;
    while i < lines.len() {
        let key = normalized(lines[i]);
        let mut j = i + 1;
        while j < lines.len() && normalized(lines[j]) == key {
            j += 1;
        }
        let run = j - i;
        if run >= min_run && !lines[i].trim().is_empty() {
            out.push(lines[i].to_string());
            out.push(format!("[{} similar lines hidden]", run - 2));
            out.push(lines[j - 1].to_string());
            hidden += run - 2;
            runs += 1;
        } else {
            out.extend(lines[i..j].iter().map(|l| l.to_string()));
        }
        i = j;
    }
    (out, hidden, runs)
}

/// Keeps a head and a tail within the token budget. Returns the text and the
/// number of lines cut.
fn cap(lines: &[String], cfg: &ShapeCfg, see_log: &dyn Fn(&str) -> String) -> (String, usize) {
    let joined = lines.join("\n");
    let budget = (cfg.cap_tokens as usize).max(64);
    if estimate_tokens(&joined) <= budget {
        return (joined, 0);
    }
    let head_budget = (budget as f32 * cfg.head_ratio.clamp(0.0, 1.0)) as usize;
    let tail_budget = budget - head_budget;

    let mut head_end = 0;
    let mut used = 0;
    while head_end < lines.len() {
        let t = estimate_tokens(&lines[head_end]) + 1;
        if used + t > head_budget {
            break;
        }
        used += t;
        head_end += 1;
    }
    let mut tail_start = lines.len();
    used = 0;
    while tail_start > head_end {
        let t = estimate_tokens(&lines[tail_start - 1]) + 1;
        if used + t > tail_budget {
            break;
        }
        used += t;
        tail_start -= 1;
    }

    if head_end == 0 && tail_start == lines.len() {
        // Not even one whole line fits: cut characters instead.
        return (cut_middle(&joined, budget * 4, cfg.head_ratio, see_log), 0);
    }
    let cut = tail_start - head_end;
    let mut parts: Vec<String> = lines[..head_end].to_vec();
    if cut > 0 {
        parts.push(format!("[{}]", see_log(&format!("{cut} lines hidden"))));
    }
    parts.extend_from_slice(&lines[tail_start..]);
    let mut text = parts.join("\n");

    // A few huge lines (minified files, long JSON) can still exceed the budget.
    let max_bytes = budget * 4;
    if text.len() > max_bytes {
        text = cut_middle(&text, max_bytes, cfg.head_ratio, see_log);
    }
    (text, cut)
}

fn cut_middle(
    text: &str,
    max_bytes: usize,
    head_ratio: f32,
    see_log: &dyn Fn(&str) -> String,
) -> String {
    let head = floor_char(text, (max_bytes as f32 * head_ratio) as usize);
    let tail = ceil_char(text, text.len() - (max_bytes - head));
    format!(
        "{}\n[{}]\n{}",
        &text[..head],
        see_log(&format!(
            "{} characters hidden",
            text[head..tail].chars().count()
        )),
        &text[tail..]
    )
}

fn floor_char(s: &str, mut i: usize) -> usize {
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil_char(s: &str, mut i: usize) -> usize {
    while !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

#[cfg(test)]
mod tests;
