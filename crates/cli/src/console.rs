//! The interactive session's look: the one colour of the logo's fur on the
//! terminal's own black.
//!
//! Every tool call is one line (`$ cli git status`), its result one dim
//! line under it, the agent's reply under its name, and a status line
//! after each turn. Colour is used only on a terminal and never when
//! `NO_COLOR` is set; piped output stays plain, so scripts that read the
//! reply from stdout see exactly the text.

use std::io::{IsTerminal, Write};
use std::time::Duration;

use serde_json::Value;

#[derive(Clone, Copy)]
pub(crate) struct Style {
    color: bool,
    truecolor: bool,
}

impl Style {
    /// Colour for stderr (where the live lines go).
    pub(crate) fn for_stderr() -> Style {
        Style::detect(std::io::stderr().is_terminal())
    }

    /// Colour for stdout (where the reply goes).
    pub(crate) fn for_stdout() -> Style {
        Style::detect(std::io::stdout().is_terminal())
    }

    fn detect(is_tty: bool) -> Style {
        let no_color = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
        let dumb = std::env::var("TERM").map(|t| t == "dumb").unwrap_or(false);
        let truecolor = std::env::var("COLORTERM")
            .map(|c| c.contains("truecolor") || c.contains("24bit"))
            .unwrap_or(false);
        Style {
            color: is_tty && !no_color && !dumb,
            truecolor,
        }
    }

    #[cfg(test)]
    pub(crate) fn plain() -> Style {
        Style {
            color: false,
            truecolor: false,
        }
    }

    fn wrap(&self, code: &str, s: &str) -> String {
        if self.color {
            format!("\x1b[{code}m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    /// The one theme colour: the mascot's red-orange, deepened so it does
    /// not glare on black (#d4573f; the nearest of the 256 colours otherwise).
    pub(crate) fn orange(&self, s: &str) -> String {
        if self.truecolor {
            self.wrap("38;2;212;87;63", s)
        } else {
            self.wrap("38;5;167", s)
        }
    }
    pub(crate) fn bold(&self, s: &str) -> String {
        self.wrap("1", s)
    }
    pub(crate) fn dim(&self, s: &str) -> String {
        self.wrap("2", s)
    }
}

/// The prompt marker.
pub(crate) fn prompt(st: &Style) -> String {
    format!("{} ", st.orange("→"))
}

/// The session's first lines.
pub(crate) fn banner(
    st: &Style,
    agent: &str,
    version: &str,
    model: &str,
    branch: &str,
    extra: &[String],
) {
    let mut parts = vec![st.bold(&st.orange("gitagent")), st.bold(agent)];
    if !version.is_empty() {
        parts.push(st.dim(&format!("v{version}")));
    }
    parts.push(model.to_string());
    if !branch.is_empty() {
        parts.push(branch.to_string());
    }
    eprintln!("{}", parts.join(&st.dim(" · ")));
    for line in extra {
        eprintln!("{}", st.dim(line));
    }
    eprintln!(
        "{}",
        st.dim("/help for commands · /quit to exit · Ctrl-C stops the current turn")
    );
}

/// Prints the live lines for a run's events.
pub(crate) fn event_line(st: &Style, e: &Value) -> Option<String> {
    match e["event"].as_str()? {
        "tool_call" => {
            let name = e["name"].as_str().unwrap_or("");
            let args = e["arguments"].as_object();
            Some(format!(
                "{} {}",
                st.orange("$"),
                tool_line(name, args.map(|a| Value::Object(a.clone())).as_ref())
            ))
        }
        "tool_result" => {
            let content = e["content"].as_str().unwrap_or("");
            let lines = content.lines().filter(|l| !l.trim().is_empty()).count();
            let first = content
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("")
                .trim();
            let first = cut(first, 100);
            let is_error = e["is_error"] == Value::Bool(true);
            let tail = match (is_error, lines) {
                (true, _) => format!("{} {}", st.orange("✗"), first),
                (false, 0) => st.dim("(no output)"),
                (false, 1) => st.dim(&first),
                (false, n) => st.dim(&format!("{first} · {n} lines")),
            };
            Some(format!("  {tail}"))
        }
        "model_text" => {
            let reasoning = e["reasoning"].as_str().unwrap_or("");
            (!reasoning.trim().is_empty()).then(|| st.dim("∴ thinking"))
        }
        "blocked" => {
            let reason = e["reason"].as_str().unwrap_or("blocked");
            Some(format!(
                "{} {}",
                st.orange("!"),
                st.bold(&format!("blocked: {reason}"))
            ))
        }
        "steer" => Some(st.dim(&format!("↪ {}", e["text"].as_str().unwrap_or("")))),
        "guard" if e["kind"] == "compaction" => Some(st.dim("· context compacted")),
        _ => None,
    }
}

/// `cli git status`, `read path`, `write path`, …: the tool as a person
/// would say it, using 1.x's names.
pub(crate) fn tool_line(name: &str, args: Option<&Value>) -> String {
    let a = args.cloned().unwrap_or(Value::Null);
    let s = |k: &str| a[k].as_str().unwrap_or("").to_string();
    let shown = gitagent_loader::v1::v1_name(name);
    match name {
        "exec" => format!("cli {}", cut(&s("cmd").replace('\n', " "), 120)),
        "write_stdin" => format!("cli ↳ {}", cut(&s("chars").replace('\n', "⏎"), 60)),
        "read_file" => format!("read {}", s("path")),
        "write_file" => format!("write {}", s("path")),
        "str_replace" | "apply_patch" => format!("edit {}", s("path")),
        "memory" => format!("memory {}", s("action")),
        "skill" => format!("skill {}", s("name")),
        "task_complete" => "done".to_string(),
        _ => {
            let short: Vec<String> = a
                .as_object()
                .into_iter()
                .flatten()
                .map(|(k, v)| match v {
                    Value::String(t) => format!("{k}={}", cut(t, 40)),
                    other => format!("{k}={}", cut(&other.to_string(), 40)),
                })
                .collect();
            if short.is_empty() {
                shown.to_string()
            } else {
                format!("{shown} {}", short.join(" "))
            }
        }
    }
}

/// The agent's reply, under its name, with the light markdown a terminal
/// can show: bold, inline code, headings. Plain when stdout is not a terminal.
pub(crate) fn reply(st: &Style, agent: &str, text: &str) {
    let mut out = std::io::stdout().lock();
    if !st.color {
        let _ = writeln!(out, "{text}");
        return;
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "{}", st.bold(&st.orange(agent)));
    for line in text.lines() {
        let line = if let Some(h) = line.strip_prefix("# ").or_else(|| line.strip_prefix("## ")) {
            st.bold(h)
        } else {
            inline(st, line)
        };
        let _ = writeln!(out, "  {line}");
    }
    let _ = writeln!(out);
}

fn inline(st: &Style, line: &str) -> String {
    let mut out = String::new();
    let mut rest = line;
    while let Some(i) = rest.find("**").or_else(|| rest.find('`')) {
        let (before, at) = rest.split_at(i);
        out.push_str(before);
        if let Some(after) = at.strip_prefix("**") {
            match after.find("**") {
                Some(j) => {
                    out.push_str(&st.bold(&after[..j]));
                    rest = &after[j + 2..];
                }
                None => {
                    out.push_str(at);
                    rest = "";
                }
            }
        } else {
            let after = &at[1..];
            match after.find('`') {
                Some(j) => {
                    out.push_str(&st.bold(&after[..j]));
                    rest = &after[j + 1..];
                }
                None => {
                    out.push_str(at);
                    rest = "";
                }
            }
        }
    }
    out.push_str(rest);
    out
}

/// The line after a turn: outcome, turns, time, tokens.
pub(crate) fn status(
    st: &Style,
    outcome: &str,
    turns: u32,
    took: Duration,
    input: u64,
    output: u64,
) {
    let ok = outcome == "completed";
    let mark = if ok {
        st.orange("●")
    } else {
        st.orange("✗")
    };
    eprintln!(
        "{mark} {}",
        st.dim(&format!(
            "{outcome} · {turns} turn{} · {:.1}s · {} in · {} out",
            if turns == 1 { "" } else { "s" },
            took.as_secs_f64(),
            group(input),
            group(output)
        ))
    );
}

fn group(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub(crate) fn cut(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn lines_read_like_a_person_would_say_them() {
        let st = Style::plain();
        assert_eq!(
            event_line(
                &st,
                &json!({"event": "tool_call", "name": "exec", "arguments": {"cmd": "git status"}})
            )
            .unwrap(),
            "$ cli git status"
        );
        assert_eq!(
            event_line(&st, &json!({"event": "tool_call", "name": "write_file", "arguments": {"path": "a.txt", "content": "x"}})).unwrap(),
            "$ write a.txt"
        );
        assert_eq!(
            event_line(
                &st,
                &json!({"event": "tool_call", "name": "shout", "arguments": {"text": "hi"}})
            )
            .unwrap(),
            "$ shout text=hi"
        );
        assert_eq!(
            event_line(&st, &json!({"event": "tool_result", "name": "exec", "is_error": false, "content": "a\nb\nc\n"})).unwrap(),
            "  a · 3 lines"
        );
        assert_eq!(
            event_line(&st, &json!({"event": "tool_result", "name": "exec", "is_error": true, "content": "boom"})).unwrap(),
            "  ✗ boom"
        );
        assert_eq!(
            event_line(
                &st,
                &json!({"event": "blocked", "kind": "hook_block", "reason": "rm is not allowed"})
            )
            .unwrap(),
            "! blocked: rm is not allowed"
        );
        assert!(event_line(&st, &json!({"event": "turn_start"})).is_none());
        assert_eq!(group(1234567), "1,234,567");
        assert_eq!(group(84), "84");
        assert_eq!(cut("abcdef", 4), "abc…");
    }

    #[test]
    fn colour_is_only_for_terminals() {
        let st = Style {
            color: true,
            truecolor: false,
        };
        assert_eq!(st.orange("x"), "\x1b[38;5;167mx\x1b[0m");
        assert_eq!(
            inline(&st, "a **b** `c` d"),
            "a \x1b[1mb\x1b[0m \x1b[1mc\x1b[0m d"
        );
        assert_eq!(inline(&Style::plain(), "a **b** `c`"), "a b c");
        assert_eq!(inline(&st, "open ** here"), "open ** here");
    }
}
