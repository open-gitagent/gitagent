//! The prompt's line editor, for a terminal: as you type `/` the matching
//! commands appear under the line (Tab or → at the end completes the first,
//! ↑/↓ pick another, Enter sends), `@` does the same for workflows, and
//! ↑/↓ on an ordinary line walk this session's history. Ctrl-C clears the
//! line or, when it is empty, ends the session like Ctrl-D.
//!
//! The terminal is put in raw mode only while a line is being read and is
//! restored after, also if the task is cancelled. Without a terminal the
//! caller reads plain lines instead.

use std::io::{Read, Write};

use crate::console::Style;

/// A command or workflow the editor can complete: the text and one line
/// about it.
#[derive(Clone)]
pub(crate) struct Choice {
    pub text: String,
    pub about: String,
}

/// What the person typed, or `None` at end of input.
pub(crate) async fn read_line(
    st: Style,
    prompt: String,
    choices: Vec<Choice>,
    history: Vec<String>,
) -> Option<String> {
    tokio::task::spawn_blocking(move || {
        let _raw = RawMode::enter()?;
        let mut tty = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .ok()?;
        let mut ed = Editor::new(st, prompt, choices, history);
        ed.draw(&mut tty);
        // One byte at a time, on purpose: a buffered reader would take the
        // next line with it and drop it when this call returns.
        #[allow(clippy::unbuffered_bytes)]
        let mut bytes = tty.try_clone().ok()?.bytes();
        loop {
            let key = read_key(&mut bytes)?;
            match ed.key(key) {
                Step::Edit => ed.draw(&mut tty),
                Step::Submit(line) => {
                    ed.finish(&mut tty);
                    return Some(line);
                }
                Step::End => {
                    ed.finish(&mut tty);
                    let _ = writeln!(tty);
                    return None;
                }
            }
        }
    })
    .await
    .ok()
    .flatten()
}

/// Raw mode on the controlling terminal, set with `stty` (the workspace
/// allows no unsafe code, so no termios calls) and undone on drop.
struct RawMode {
    saved: String,
}

impl RawMode {
    fn enter() -> Option<RawMode> {
        let saved = stty(&["-g"])?;
        let saved = saved.trim().to_string();
        if saved.is_empty() {
            return None;
        }
        stty(&["raw", "-echo"])?;
        Some(RawMode { saved })
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = stty(&[self.saved.as_str()]);
    }
}

/// The terminal's width in columns: `stty size`, else `COLUMNS`, else 100.
pub(crate) fn terminal_width() -> usize {
    stty(&["size"])
        .and_then(|s| s.split_whitespace().nth(1)?.parse().ok())
        .or_else(|| std::env::var("COLUMNS").ok()?.trim().parse().ok())
        .filter(|w: &usize| *w > 0)
        .unwrap_or(100)
}

/// Runs `stty` on the controlling terminal; its stdout, or `None` when
/// there is no terminal or no stty.
fn stty(args: &[&str]) -> Option<String> {
    let tty = std::fs::File::open("/dev/tty").ok()?;
    let out = std::process::Command::new("stty")
        .args(args)
        .stdin(tty)
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

#[derive(Debug, PartialEq)]
enum Key {
    Char(char),
    Enter,
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    Tab,
    Escape,
    CtrlC,
    CtrlD,
    CtrlU,
    CtrlW,
    CtrlL,
}

fn read_key(bytes: &mut impl Iterator<Item = std::io::Result<u8>>) -> Option<Key> {
    let b = bytes.next()?.ok()?;
    Some(match b {
        b'\r' | b'\n' => Key::Enter,
        0x7f | 0x08 => Key::Backspace,
        b'\t' => Key::Tab,
        0x01 => Key::Home,
        0x05 => Key::End,
        0x03 => Key::CtrlC,
        0x04 => Key::CtrlD,
        0x15 => Key::CtrlU,
        0x17 => Key::CtrlW,
        0x0c => Key::CtrlL,
        0x1b => {
            // An escape sequence, or a lone Escape.
            let (Some(Ok(b'[')) | Some(Ok(b'O'))) = bytes.next() else {
                return Some(Key::Escape);
            };
            let mut seq = String::new();
            loop {
                let c = bytes.next()?.ok()? as char;
                seq.push(c);
                if c.is_ascii_alphabetic() || c == '~' {
                    break;
                }
            }
            match seq.as_str() {
                "A" => Key::Up,
                "B" => Key::Down,
                "C" => Key::Right,
                "D" => Key::Left,
                "H" | "1~" | "7~" => Key::Home,
                "F" | "4~" | "8~" => Key::End,
                "3~" => Key::Delete,
                _ => Key::Escape,
            }
        }
        b if b < 0x20 => return read_key(bytes),
        b => {
            // UTF-8: the first byte says how many follow.
            let n = match b {
                0x00..=0x7f => 0,
                0xc0..=0xdf => 1,
                0xe0..=0xef => 2,
                _ => 3,
            };
            let mut buf = vec![b];
            for _ in 0..n {
                buf.push(bytes.next()?.ok()?);
            }
            Key::Char(String::from_utf8_lossy(&buf).chars().next().unwrap_or(' '))
        }
    })
}

enum Step {
    Edit,
    Submit(String),
    End,
}

struct Editor {
    st: Style,
    prompt: String,
    prompt_width: usize,
    choices: Vec<Choice>,
    history: Vec<String>,
    history_at: Option<usize>,
    buf: Vec<char>,
    cursor: usize,
    /// Which suggestion is picked, when the menu is showing.
    pick: usize,
    /// How many lines the last draw used under the input.
    drawn_below: usize,
}

const MENU_ROWS: usize = 6;

impl Editor {
    fn new(st: Style, prompt: String, choices: Vec<Choice>, history: Vec<String>) -> Editor {
        let prompt_width = strip_ansi(&prompt).chars().count();
        Editor {
            st,
            prompt,
            prompt_width,
            choices,
            history,
            history_at: None,
            buf: Vec::new(),
            cursor: 0,
            pick: 0,
            drawn_below: 0,
        }
    }

    fn text(&self) -> String {
        self.buf.iter().collect()
    }

    /// The suggestions for what is typed so far: commands for `/…`,
    /// workflows for `@…`, none once a space is typed.
    fn suggestions(&self) -> Vec<&Choice> {
        let t = self.text();
        if t.is_empty() || t.contains(' ') || !(t.starts_with('/') || t.starts_with('@')) {
            return Vec::new();
        }
        let mut v: Vec<&Choice> = self
            .choices
            .iter()
            .filter(|c| c.text.starts_with(&t) && c.text != t)
            .collect();
        if v.is_empty() {
            // Not a prefix: match anywhere, so `/sch` finds `/schedules`
            // and `/mem` finds `/memory`.
            let needle = t[1..].to_lowercase();
            v = self
                .choices
                .iter()
                .filter(|c| {
                    c.text.starts_with(&t[..1])
                        && c.text[1..].to_lowercase().contains(&needle)
                        && c.text != t
                })
                .collect();
        }
        v.truncate(MENU_ROWS);
        v
    }

    fn key(&mut self, key: Key) -> Step {
        let menu = !self.suggestions().is_empty();
        match key {
            Key::Char(c) => {
                self.buf.insert(self.cursor, c);
                self.cursor += 1;
                self.pick = 0;
                self.history_at = None;
            }
            Key::Enter => {
                let line = self.text();
                return Step::Submit(line);
            }
            Key::Backspace => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    self.buf.remove(self.cursor);
                    self.pick = 0;
                }
            }
            Key::Delete => {
                if self.cursor < self.buf.len() {
                    self.buf.remove(self.cursor);
                }
            }
            Key::Left => self.cursor = self.cursor.saturating_sub(1),
            Key::Right => {
                if self.cursor < self.buf.len() {
                    self.cursor += 1;
                } else if menu {
                    self.complete();
                }
            }
            Key::Home => self.cursor = 0,
            Key::End => self.cursor = self.buf.len(),
            Key::Tab => {
                if menu {
                    self.complete();
                }
            }
            Key::Up => {
                if menu {
                    let n = self.suggestions().len();
                    self.pick = (self.pick + n - 1) % n;
                } else {
                    self.history_step(-1);
                }
            }
            Key::Down => {
                if menu {
                    let n = self.suggestions().len();
                    self.pick = (self.pick + 1) % n;
                } else {
                    self.history_step(1);
                }
            }
            Key::Escape => {
                if menu {
                    // Leave the menu by moving past it with a space.
                } else {
                    self.history_at = None;
                }
            }
            Key::CtrlU => {
                self.buf.clear();
                self.cursor = 0;
            }
            Key::CtrlW => {
                let mut i = self.cursor;
                while i > 0 && self.buf[i - 1] == ' ' {
                    i -= 1;
                }
                while i > 0 && self.buf[i - 1] != ' ' {
                    i -= 1;
                }
                self.buf.drain(i..self.cursor);
                self.cursor = i;
            }
            Key::CtrlL => {}
            Key::CtrlC => {
                if self.buf.is_empty() {
                    return Step::End;
                }
                self.buf.clear();
                self.cursor = 0;
            }
            Key::CtrlD => {
                if self.buf.is_empty() {
                    return Step::End;
                }
                if self.cursor < self.buf.len() {
                    self.buf.remove(self.cursor);
                }
            }
        }
        Step::Edit
    }

    fn complete(&mut self) {
        let picked = self.suggestions().get(self.pick).map(|c| c.text.clone());
        if let Some(text) = picked {
            self.buf = text.chars().collect();
            if !text.ends_with(':') {
                self.buf.push(' ');
            }
            self.cursor = self.buf.len();
            self.pick = 0;
        }
    }

    fn history_step(&mut self, dir: i32) {
        if self.history.is_empty() {
            return;
        }
        let at = match (self.history_at, dir) {
            (None, -1) => self.history.len() - 1,
            (None, _) => return,
            (Some(0), -1) => 0,
            (Some(i), -1) => i - 1,
            (Some(i), _) if i + 1 >= self.history.len() => {
                self.history_at = None;
                self.buf.clear();
                self.cursor = 0;
                return;
            }
            (Some(i), _) => i + 1,
        };
        self.history_at = Some(at);
        self.buf = self.history[at].chars().collect();
        self.cursor = self.buf.len();
    }

    /// Redraws the input line and the menu under it, then puts the cursor
    /// back where it belongs.
    fn draw(&mut self, tty: &mut std::fs::File) {
        let st = self.st;
        let mut out = String::new();
        // Clear from the start of the line to the end of the screen.
        out.push_str("\r\x1b[J");
        out.push_str(&self.prompt);
        out.push_str(&self.text());
        let menu = self.suggestions();
        let mut rows = 0;
        for (i, c) in menu.iter().enumerate() {
            out.push_str("\r\n");
            let (mark, name) = if i == self.pick {
                (st.orange("▸"), st.bold(&c.text))
            } else {
                (" ".to_string(), c.text.clone())
            };
            let pad = " ".repeat(22usize.saturating_sub(c.text.chars().count()).max(2));
            out.push_str(&format!("  {mark} {name}{pad}{}", st.dim(&c.about)));
            rows += 1;
        }
        if rows > 0 {
            out.push_str("\r\n");
            out.push_str(&st.dim("  Tab completes · ↑↓ pick · Enter sends"));
            rows += 1;
            // Back up to the input line.
            out.push_str(&format!("\x1b[{rows}A"));
        }
        out.push_str(&format!("\r\x1b[{}C", self.prompt_width + self.cursor));
        self.drawn_below = rows;
        let _ = tty.write_all(out.as_bytes());
        let _ = tty.flush();
    }

    /// Leaves the input line as it is and clears the menu under it.
    fn finish(&mut self, tty: &mut std::fs::File) {
        let mut out = String::from("\r\x1b[J");
        out.push_str(&self.prompt);
        out.push_str(&self.text());
        out.push_str("\r\n");
        let _ = tty.write_all(out.as_bytes());
        let _ = tty.flush();
    }
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::new();
    let mut in_escape = false;
    for c in s.chars() {
        match (in_escape, c) {
            (true, 'm') => in_escape = false,
            (true, _) => {}
            (false, '\x1b') => in_escape = true,
            (false, _) => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor() -> Editor {
        let choices = [
            "/memory",
            "/skills",
            "/schedules",
            "/skill:",
            "/quit",
            "@shout-twice",
        ]
        .iter()
        .map(|t| Choice {
            text: t.to_string(),
            about: String::new(),
        })
        .collect();
        Editor::new(
            Style::plain(),
            "→ ".into(),
            choices,
            vec!["first".into(), "second".into()],
        )
    }

    fn type_in(ed: &mut Editor, s: &str) {
        for c in s.chars() {
            ed.key(Key::Char(c));
        }
    }

    #[test]
    fn slash_shows_matching_commands_and_tab_completes() {
        let mut ed = editor();
        assert!(ed.suggestions().is_empty());
        type_in(&mut ed, "/");
        assert_eq!(ed.suggestions().len(), 5);
        type_in(&mut ed, "s");
        let names: Vec<&str> = ed.suggestions().iter().map(|c| c.text.as_str()).collect();
        assert_eq!(names, vec!["/skills", "/schedules", "/skill:"]);
        ed.key(Key::Down);
        ed.key(Key::Tab);
        assert_eq!(ed.text(), "/schedules ");
        assert!(ed.suggestions().is_empty(), "a space ends the menu");
        // Matching inside the word too.
        let mut ed = editor();
        type_in(&mut ed, "/mem");
        assert_eq!(ed.suggestions()[0].text, "/memory");
        let mut ed = editor();
        type_in(&mut ed, "/ched");
        assert_eq!(ed.suggestions()[0].text, "/schedules");
        // A skill command keeps the colon, no space.
        let mut ed = editor();
        type_in(&mut ed, "/skill:");
        assert!(ed.suggestions().is_empty());
        let mut ed = editor();
        type_in(&mut ed, "/skil");
        ed.key(Key::Down);
        ed.key(Key::Right);
        assert_eq!(ed.text(), "/skill:");
        // Workflows after @.
        let mut ed = editor();
        type_in(&mut ed, "@sh");
        assert_eq!(ed.suggestions()[0].text, "@shout-twice");
    }

    #[test]
    fn editing_keys_and_history() {
        let mut ed = editor();
        type_in(&mut ed, "hello wrld");
        ed.key(Key::Left);
        ed.key(Key::Left);
        ed.key(Key::Left);
        ed.key(Key::Char('o'));
        assert_eq!(ed.text(), "hello world");
        ed.key(Key::Home);
        ed.key(Key::Delete);
        assert_eq!(ed.text(), "ello world");
        ed.key(Key::End);
        ed.key(Key::CtrlW);
        assert_eq!(ed.text(), "ello ");
        ed.key(Key::CtrlU);
        assert_eq!(ed.text(), "");
        ed.key(Key::Up);
        assert_eq!(ed.text(), "second");
        ed.key(Key::Up);
        assert_eq!(ed.text(), "first");
        ed.key(Key::Down);
        ed.key(Key::Down);
        assert_eq!(ed.text(), "");
        assert!(matches!(ed.key(Key::CtrlC), Step::End));
        type_in(&mut ed, "x");
        assert!(matches!(ed.key(Key::CtrlC), Step::Edit));
        assert_eq!(ed.text(), "");
        type_in(&mut ed, "send me");
        assert!(matches!(ed.key(Key::Enter), Step::Submit(s) if s == "send me"));
    }

    #[test]
    fn keys_are_decoded() {
        let decode = |b: &[u8]| read_key(&mut b.iter().map(|x| Ok(*x))).unwrap();
        assert_eq!(decode(b"a"), Key::Char('a'));
        assert_eq!(decode("é".as_bytes()), Key::Char('é'));
        assert_eq!(decode(b"\r"), Key::Enter);
        assert_eq!(decode(b"\x7f"), Key::Backspace);
        assert_eq!(decode(b"\x1b[A"), Key::Up);
        assert_eq!(decode(b"\x1b[3~"), Key::Delete);
        assert_eq!(decode(b"\x1bOH"), Key::Home);
        assert_eq!(decode(b"\x03"), Key::CtrlC);
        assert_eq!(strip_ansi("\x1b[38;5;202m→\x1b[0m "), "→ ");
    }
}
