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

    pub(crate) fn is_color(&self) -> bool {
        self.color
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

/// The session's first lines: a boxed welcome with the build, the agent,
/// its model and branch, and where it lives, then the tools and skills.
pub(crate) fn banner(
    st: &Style,
    agent: &str,
    version: &str,
    model: &str,
    branch: &str,
    dir: &str,
    extra: &[String],
) {
    // (plain text, how to colour it) per line; width comes from the plain text.
    let mut agent_line = agent.to_string();
    if !version.is_empty() {
        agent_line.push_str(&format!(" v{version}"));
    }
    agent_line.push_str(&format!(" · {model}"));
    if !branch.is_empty() {
        agent_line.push_str(&format!(" · {branch}"));
    }
    let title = format!("gitagent v{}", env!("CARGO_PKG_VERSION"));
    let hint = "/help for commands · /quit to exit · Ctrl-C stops the current turn";
    let rows: Vec<(String, u8)> = vec![
        (title.clone(), 1),
        (String::new(), 0),
        (agent_line, 2),
        (dir.to_string(), 3),
        (String::new(), 0),
        (hint.to_string(), 3),
    ];
    let width = rows
        .iter()
        .map(|(t, _)| t.chars().count())
        .max()
        .unwrap_or(0)
        .min(100);
    // Narrow terminals: the box (its text plus six columns of frame) fits.
    let width = if st.color {
        width.min(crate::lineedit::terminal_width().saturating_sub(6).max(20))
    } else {
        width
    };
    let bar = "─".repeat(width + 4);
    let mut box_lines = vec![st.dim(&format!("╭{bar}╮"))];
    for (text, kind) in rows {
        let text = cut(&text, width);
        let shown = match kind {
            1 => format!("{} {}", st.orange("●"), st.bold(&text)),
            3 => st.dim(&text),
            _ => text.clone(),
        };
        // The dot adds two columns to the first row.
        let used = text.chars().count() + if kind == 1 { 2 } else { 0 };
        let pad = " ".repeat(width.saturating_sub(used));
        box_lines.push(format!("{}  {shown}{pad}  {}", st.dim("│"), st.dim("│")));
    }
    box_lines.push(st.dim(&format!("╰{bar}╯")));
    if st.color {
        for row in big_text("gitagent") {
            eprintln!("{}", st.orange(&row));
        }
        eprintln!();
    }
    for line in &box_lines {
        eprintln!("{line}");
    }
    for line in extra {
        eprintln!("{}", st.dim(line));
    }
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
        "plan" => {
            let s = |k: &str| e[k].as_str().unwrap_or("");
            Some(match s("action") {
                "phase_start" => format!(
                    "{} {}",
                    st.orange("⇶"),
                    st.bold(&format!(
                        "phase {}/{} {} · {} task{} at once",
                        e["phase"],
                        e["of"],
                        s("name"),
                        e["tasks"],
                        if e["tasks"] == 1 { "" } else { "s" }
                    ))
                ),
                "task_start" => format!("  {} {}", st.dim("◌"), st.dim(s("description"))),
                "task_end" => format!(
                    "  {} {} {}",
                    if e["ok"] == true {
                        st.orange("✓")
                    } else {
                        st.orange("✗")
                    },
                    s("description"),
                    st.dim(&format!("— {}", cut(s("result"), 80)))
                ),
                "reconcile_start" => {
                    format!("{} {}", st.orange("⇶"), st.bold("reconciling the results"))
                }
                "reconcile_end" => {
                    format!("  {} {}", st.orange("●"), st.dim(&cut(s("result"), 100)))
                }
                _ => return None,
            })
        }
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
            "{outcome} · {turns} model call{} · {:.1}s · {} in · {} out",
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

/// A word in the classic line-drawn letters, six rows tall. Only the
/// letters of "gitagent" are drawn; others come out as a blank column.
fn big_text(word: &str) -> Vec<String> {
    const G: [&str; 6] = [
        "       ", "  __ _ ", " / _` |", "| (_| |", " \\__, |", " |___/ ",
    ];
    const I: [&str; 6] = [" _ ", "(_)", "| |", "| |", "|_|", "   "];
    const T: [&str; 6] = [" _   ", "| |_ ", "| __|", "| |_ ", " \\__|", "     "];
    const A: [&str; 6] = [
        "       ", "  __ _ ", " / _` |", "| (_| |", " \\__,_|", "       ",
    ];
    const E: [&str; 6] = ["      ", "  ___ ", " / _ \\", "|  __/", " \\___|", "      "];
    const N: [&str; 6] = [
        "       ", " _ __  ", "| '_ \\ ", "| | | |", "|_| |_|", "       ",
    ];
    let glyph = |c: char| -> Option<[&str; 6]> {
        Some(match c {
            'g' => G,
            'i' => I,
            't' => T,
            'a' => A,
            'e' => E,
            'n' => N,
            _ => return None,
        })
    };
    let mut rows: Vec<Vec<char>> = vec![Vec::new(); 6];
    for c in word.chars() {
        join(&mut rows, &glyph(c).unwrap_or(["  "; 6]));
    }
    rows.iter()
        .map(|r| r.iter().collect::<String>().trim_end().to_string())
        .collect()
}

/// Adds a glyph to the right of `rows`, pulled in until the letters touch
/// and one column further where the touching characters are the same.
fn join(rows: &mut [Vec<char>], glyph: &[&str; 6]) {
    let glyph: Vec<Vec<char>> = glyph.iter().map(|r| r.chars().collect()).collect();
    if rows.iter().all(Vec::is_empty) {
        for (r, g) in rows.iter_mut().zip(&glyph) {
            *r = g.clone();
        }
        return;
    }
    let gaps: Vec<(usize, usize)> = rows
        .iter()
        .zip(&glyph)
        .map(|(r, g)| {
            (
                r.iter().rev().take_while(|c| **c == ' ').count(),
                g.iter().take_while(|c| **c == ' ').count(),
            )
        })
        .collect();
    let mut shift = gaps.iter().map(|(t, l)| t + l).min().unwrap_or(0);
    let touching_match = rows.iter().zip(&glyph).zip(&gaps).all(|((r, g), (t, l))| {
        t + l > shift || g.len() == *l || r.len() == *t || r[r.len() - 1 - t] == g[*l]
    });
    if touching_match && rows.iter().zip(&gaps).any(|(r, (t, _))| r.len() > *t) {
        shift += 1;
    }
    for ((r, g), (t, l)) in rows.iter_mut().zip(&glyph).zip(&gaps) {
        let mut left = shift;
        let drop_right = (*t).min(left);
        r.truncate(r.len() - drop_right);
        left -= drop_right;
        let skip = (*l).min(left);
        left -= skip;
        // What is left is the one merged column: the glyph's copy goes.
        r.extend(g.iter().skip(skip + left));
    }
}

/// Set while a task board owns the terminal, so the spinner stays quiet.
static BOARD_ACTIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The live view of a `plan` run: one line per task, redrawn in place as
/// tasks start and finish, with the phase and the reconciling step.
#[derive(Clone)]
pub(crate) struct Board {
    st: Style,
    inner: std::sync::Arc<std::sync::Mutex<BoardState>>,
}

#[derive(Default)]
struct BoardState {
    active: bool,
    phase: usize,
    of: usize,
    phase_name: String,
    /// Finished phases: (name, tasks, failed, stopped at the turn limit).
    earlier: Vec<(String, usize, usize, usize)>,
    /// Tasks in the current phase, started or not.
    phase_total: usize,
    /// Columns to draw in; longer lines are cut so each takes one row.
    width: usize,
    /// (running at once, ceiling, rate limits so far), once a rate limit
    /// has narrowed the window.
    window: Option<(usize, usize, u64)>,
    tasks: Vec<TaskRow>,
    reconcile: Option<(bool, Option<bool>, String, std::time::Instant)>,
    drawn: usize,
    frame: usize,
    started: Option<std::time::Instant>,
}

struct TaskRow {
    id: String,
    description: String,
    agent: String,
    ok: Option<bool>,
    /// It ran out of turns; its result is partial.
    partial: bool,
    result: String,
    started: std::time::Instant,
    took: Option<Duration>,
}

const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

impl Board {
    pub(crate) fn new(st: Style) -> Board {
        Board {
            st,
            inner: std::sync::Arc::new(std::sync::Mutex::new(BoardState::default())),
        }
    }

    /// Folds a `plan` event in and redraws. Returns false for other events.
    pub(crate) fn on_event(&self, e: &Value) -> bool {
        if e["event"] != "plan" {
            return false;
        }
        let s = |k: &str| e[k].as_str().unwrap_or("").to_string();
        let mut b = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let now = std::time::Instant::now();
        match e["action"].as_str().unwrap_or("") {
            "phase_start" => {
                if !b.active {
                    *b = BoardState::default();
                    b.active = true;
                    b.started = Some(now);
                    b.width = crate::lineedit::terminal_width();
                    BOARD_ACTIVE.store(true, std::sync::atomic::Ordering::Relaxed);
                    clear_line(&self.st);
                    self.tick();
                }
                if b.phase > 0 {
                    let failed = b.tasks.iter().filter(|t| t.ok == Some(false)).count();
                    let partial = b.tasks.iter().filter(|t| t.partial).count();
                    let name = b.phase_name.clone();
                    let n = b.tasks.len();
                    b.earlier.push((name, n, failed, partial));
                    b.tasks.clear();
                }
                b.phase = e["phase"].as_u64().unwrap_or(0) as usize;
                b.of = e["of"].as_u64().unwrap_or(0) as usize;
                b.phase_name = s("name");
                b.phase_total = e["tasks"].as_u64().unwrap_or(0) as usize;
            }
            "task_start" => b.tasks.push(TaskRow {
                id: s("id"),
                description: s("description"),
                agent: s("agent"),
                ok: None,
                partial: false,
                result: String::new(),
                started: now,
                took: None,
            }),
            "task_end" => {
                let id = s("id");
                if let Some(t) = b.tasks.iter_mut().find(|t| t.id == id) {
                    t.ok = Some(e["ok"] == true);
                    t.partial = e["partial"] == true;
                    t.result = s("result");
                    t.took = Some(now.duration_since(t.started));
                }
            }
            "window" => {
                b.window = Some((
                    e["at_once"].as_u64().unwrap_or(0) as usize,
                    e["ceiling"].as_u64().unwrap_or(0) as usize,
                    e["rate_limited"].as_u64().unwrap_or(0),
                ));
            }
            "reconcile_start" => b.reconcile = Some((true, None, String::new(), now)),
            "reconcile_end" => {
                b.reconcile = Some((false, Some(e["ok"] == true), s("result"), now));
                b.active = false;
                BOARD_ACTIVE.store(false, std::sync::atomic::Ordering::Relaxed);
            }
            _ => {}
        }
        drop(b);
        self.draw();
        let mut b = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        if !b.active {
            // The final picture stays; later lines go under it.
            b.drawn = 0;
        }
        true
    }

    fn tick(&self) {
        let me = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(140)).await;
                {
                    let mut b = me.inner.lock().unwrap_or_else(|p| p.into_inner());
                    if !b.active {
                        break;
                    }
                    b.frame += 1;
                }
                me.draw();
            }
        });
    }

    fn draw(&self) {
        let mut b = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let lines = self.lines(&b);
        let mut out = String::new();
        if b.drawn > 0 {
            out.push_str(&format!("\x1b[{}A", b.drawn));
        }
        out.push_str("\r\x1b[J");
        for l in &lines {
            out.push_str(l);
            out.push('\n');
        }
        eprint!("{out}");
        let _ = std::io::stderr().flush();
        b.drawn = lines.len();
    }

    /// The board as lines, for the terminal.
    fn lines(&self, b: &BoardState) -> Vec<String> {
        let st = &self.st;
        let frame = FRAMES[b.frame % FRAMES.len()];
        let mut out = Vec::new();
        for (name, n, failed, partial) in &b.earlier {
            out.push(format!(
                "{} {}",
                st.orange("✓"),
                st.dim(&format!(
                    "{name} · {n} task{}{}{}",
                    if *n == 1 { "" } else { "s" },
                    if *failed > 0 {
                        format!(", {failed} failed")
                    } else {
                        String::new()
                    },
                    if *partial > 0 {
                        format!(", {partial} out of turns")
                    } else {
                        String::new()
                    }
                ))
            ));
        }
        let running = b.tasks.iter().filter(|t| t.ok.is_none()).count();
        let finished = b.tasks.len() - running;
        let total = b.phase_total.max(b.tasks.len());
        let elapsed = b.started.map(|s| s.elapsed().as_secs()).unwrap_or(0);
        let slowed = match b.window {
            Some((w, c, hits)) if w < c => format!(
                " · slowed to {w} at a time after {hits} rate limit{}",
                if hits == 1 { "" } else { "s" }
            ),
            Some((_, c, hits)) => format!(
                " · {hits} rate limit{}, back to {c} at a time",
                if hits == 1 { "" } else { "s" }
            ),
            None => String::new(),
        };
        out.push(format!(
            "{} {} {}",
            st.orange("⇶"),
            st.bold(&format!("phase {}/{} {}", b.phase, b.of, b.phase_name)),
            st.dim(&format!(
                "· {running} running · {finished} of {total} done · {elapsed}s{slowed}"
            ))
        ));
        const SHOW: usize = 14;
        let hidden = b.tasks.len().saturating_sub(SHOW);
        if hidden > 0 {
            let done = b.tasks[..hidden].iter().filter(|t| t.ok.is_some()).count();
            out.push(st.dim(&format!(
                "  … {hidden} earlier task{} ({done} done)",
                if hidden == 1 { "" } else { "s" }
            )));
        }
        for t in b.tasks.iter().skip(hidden) {
            let who = if t.agent.is_empty() || t.agent == "general" {
                String::new()
            } else {
                format!(" ({})", t.agent)
            };
            out.push(match t.ok {
                None => format!(
                    "  {} {}{} {}",
                    st.orange(frame),
                    t.description,
                    st.dim(&who),
                    st.dim(&format!("{}s", t.started.elapsed().as_secs()))
                ),
                Some(true) if t.partial => format!(
                    "  {} {}{} {}",
                    st.orange("◐"),
                    t.description,
                    st.dim(&who),
                    st.dim(&format!("— out of turns, partial: {}", cut(&t.result, 60)))
                ),
                Some(true) => format!(
                    "  {} {}{} {}",
                    st.orange("✓"),
                    t.description,
                    st.dim(&who),
                    st.dim(&format!("— {}", cut(&t.result, 70)))
                ),
                Some(false) => format!(
                    "  {} {}{} {}",
                    st.orange("✗"),
                    st.bold(&t.description),
                    st.dim(&who),
                    st.dim(&format!("— {}", cut(&t.result, 70)))
                ),
            });
        }
        if let Some((running, ok, result, at)) = &b.reconcile {
            out.push(if *running {
                format!(
                    "{} {} {}",
                    st.orange(frame),
                    st.bold("reconciling the results"),
                    st.dim(&format!("{}s", at.elapsed().as_secs()))
                )
            } else {
                format!(
                    "{} {} {}",
                    st.orange(if *ok == Some(true) { "●" } else { "✗" }),
                    st.bold("reconciled"),
                    st.dim(&format!("— {}", cut(result, 90)))
                )
            });
        }
        // One row per line, or the redraw would lose count of the rows.
        let width = if b.width == 0 { 100 } else { b.width };
        out.into_iter().map(|l| clip_visible(&l, width)).collect()
    }
}

/// `line` cut to fit in `width` columns, not counting colour codes, so it
/// never wraps onto a second row.
fn clip_visible(line: &str, width: usize) -> String {
    let room = width.saturating_sub(1).max(10);
    let mut visible = 0;
    let mut in_escape = false;
    for c in line.chars() {
        if in_escape {
            in_escape = c != 'm';
        } else if c == '\x1b' {
            in_escape = true;
        } else {
            visible += 1;
        }
    }
    if visible <= room {
        return line.to_string();
    }
    let mut out = String::new();
    let mut shown = 0;
    let mut in_escape = false;
    for c in line.chars() {
        if in_escape {
            out.push(c);
            in_escape = c != 'm';
        } else if c == '\x1b' {
            out.push(c);
            in_escape = true;
        } else if shown + 1 < room {
            out.push(c);
            shown += 1;
        }
    }
    out.push('…');
    out.push_str("\x1b[0m");
    out
}

/// A line that redraws itself while the model works, on a terminal only.
/// Dropping it clears the line.
pub(crate) struct Spinner {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Spinner {
    pub(crate) fn start(st: Style, label: &'static str) -> Spinner {
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(!st.color));
        if st.color {
            let flag = stop.clone();
            let started = std::time::Instant::now();
            tokio::spawn(async move {
                const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
                let mut i = 0;
                while !flag.load(std::sync::atomic::Ordering::Relaxed) {
                    if BOARD_ACTIVE.load(std::sync::atomic::Ordering::Relaxed) {
                        tokio::time::sleep(Duration::from_millis(120)).await;
                        continue;
                    }
                    let secs = started.elapsed().as_secs();
                    let line = format!(
                        "\r\x1b[2K{} {}",
                        st.orange(FRAMES[i % FRAMES.len()]),
                        st.dim(&if secs >= 2 {
                            format!("{label} · {secs}s")
                        } else {
                            label.to_string()
                        })
                    );
                    eprint!("{line}");
                    let _ = std::io::stderr().flush();
                    i += 1;
                    tokio::time::sleep(Duration::from_millis(120)).await;
                }
            });
        }
        Spinner { stop }
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        if !self.stop.swap(true, std::sync::atomic::Ordering::Relaxed)
            && !BOARD_ACTIVE.load(std::sync::atomic::Ordering::Relaxed)
        {
            eprint!("\r\x1b[2K");
            let _ = std::io::stderr().flush();
        }
    }
}

/// Clears the spinner's line before another line is printed.
pub(crate) fn clear_line(st: &Style) {
    if st.color {
        eprint!("\r\x1b[2K");
    }
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

    #[test]
    fn board_lines_are_cut_to_the_terminal_without_counting_colour() {
        let st = Style {
            color: true,
            ..Style::plain()
        };
        let line = format!("  {} {}", st.orange("✓"), "x".repeat(200));
        let cut = clip_visible(&line, 40);
        let visible: String = {
            let mut out = String::new();
            let mut esc = false;
            for c in cut.chars() {
                if esc {
                    esc = c != 'm';
                } else if c == '\x1b' {
                    esc = true;
                } else {
                    out.push(c);
                }
            }
            out
        };
        assert_eq!(visible.chars().count(), 39, "{visible}");
        assert!(visible.ends_with('…'));
        assert!(cut.ends_with("\x1b[0m"));
        let short = format!("  {} done", st.orange("✓"));
        assert_eq!(clip_visible(&short, 40), short);
    }
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
    fn the_big_word_is_six_even_rows() {
        let rows = big_text("gitagent");
        assert_eq!(rows.len(), 6);
        let widths: Vec<usize> = rows.iter().map(|r| r.chars().count()).collect();
        assert!(widths.iter().all(|w| *w <= 42), "{widths:?}");
        assert_eq!(
            rows[2], " / _` | || __|/ _` |/ _` |/ _ \\| '_ \\| __|",
            "{rows:#?}"
        );
        assert!(rows[5].starts_with(" |___/"), "{rows:#?}");
    }

    #[test]
    fn the_board_lists_phases_and_tasks() {
        let st = Style::plain();
        let b = Board::new(st);
        let now = std::time::Instant::now();
        let mut state = BoardState {
            active: true,
            phase: 2,
            of: 2,
            phase_name: "check".into(),
            earlier: vec![("read".into(), 3, 1, 0)],
            phase_total: 3,
            ..Default::default()
        };
        state.tasks.push(TaskRow {
            id: "a".into(),
            description: "check them".into(),
            agent: "worker".into(),
            ok: None,
            partial: false,
            result: String::new(),
            started: now,
            took: None,
        });
        state.tasks.push(TaskRow {
            id: "b".into(),
            description: "check more".into(),
            agent: "general".into(),
            ok: Some(true),
            partial: false,
            result: "all fine".into(),
            started: now,
            took: Some(Duration::from_secs(1)),
        });
        state.tasks.push(TaskRow {
            id: "c".into(),
            description: "big review".into(),
            agent: "general".into(),
            ok: Some(true),
            partial: true,
            result: "Now lines 400-640".into(),
            started: now,
            took: Some(Duration::from_secs(9)),
        });
        state.started = Some(now);
        let lines = b.lines(&state);
        assert_eq!(lines[0], "✓ read · 3 tasks, 1 failed");
        assert!(
            lines[1].starts_with("⇶ phase 2/2 check · 1 running · 2 of 3 done"),
            "{}",
            lines[1]
        );
        assert!(
            lines[2].starts_with("  ⠋ check them (worker)"),
            "{}",
            lines[2]
        );
        assert_eq!(lines[3], "  ✓ check more — all fine");
        assert_eq!(
            lines[4],
            "  ◐ big review — out of turns, partial: Now lines 400-640"
        );
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
