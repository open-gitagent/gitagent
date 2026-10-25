//! Interactive mode: messages are read from stdin one line at a time, in
//! conversation mode (a plain reply ends each turn). Each message continues
//! the same conversation and shell. Ctrl-C stops the current turn (and starts
//! a fresh conversation); Ctrl-D, `exit` or `/quit` ends the session.
//!
//! Commands: `/help`, `/memory`, `/skills`, `/agents`, `/tools`,
//! `/workflows`, `/skill:<name> [request]` (sends the skill's instructions
//! with the request), `@<workflow> [key=value …]` (runs a workflow),
//! `/quit`.

use std::io::Write;
use std::path::Path;

use anyhow::Result;
use gitagent_core::{Resume, RunOptions};
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::console::{self, Style};
use crate::{lineedit, Setup};

pub(crate) async fn run(s: Setup, out_dir: &Path) -> Result<()> {
    let st = Style::for_stderr();
    let out_st = Style::for_stdout();
    banner(&s, &st);
    let agent_name = s
        .spec
        .agent
        .as_ref()
        .map_or_else(|| s.spec.config.name.clone(), |a| a.manifest.name.clone());
    // Live lines for each tool call and result, on stderr; a plan's tasks
    // as a board that redraws in place on a terminal.
    let board = console::Board::new(st);
    let events = s.events.clone().also(move |e| {
        if st.is_color() && board.on_event(&e) {
            return;
        }
        if let Some(line) = console::event_line(&st, &e) {
            console::clear_line(&st);
            eprintln!("{line}");
        }
    });
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    // On a terminal, a line editor with command suggestions; piped input
    // is read line by line.
    let editing = std::io::IsTerminal::is_terminal(&std::io::stdin()) && st.is_color();
    let choices = choices(&s);
    let mut history: Vec<String> = Vec::new();
    let mut resume: Option<Resume> = None;
    let mut n = 0;
    let session_id = format!("gitagent-{}", std::process::id());
    let mcp = crate::connect_mcp(&s.spec).await;
    loop {
        let line = if editing {
            match lineedit::read_line(st, console::prompt(&st), choices.clone(), history.clone())
                .await
            {
                Some(l) => l,
                None => break,
            }
        } else {
            eprint!("{}", console::prompt(&st));
            let _ = std::io::stderr().flush();
            let Some(line) = lines.next_line().await? else {
                eprintln!();
                break;
            };
            line
        };
        let line = line.trim();
        if !line.is_empty() && history.last().map(String::as_str) != Some(line) {
            history.push(line.to_string());
        }
        if line == "/" {
            command(&s, "help");
            continue;
        }
        if line.is_empty() {
            continue;
        }
        if matches!(line, "exit" | "quit" | "/quit" | "/exit") {
            break;
        }
        if let Some(rest) = line.strip_prefix('@') {
            let mut words = rest.split_whitespace().map(str::to_string);
            let name = words.next().unwrap_or_default();
            let inputs = match gitagent_core::workflow::parse_inputs(&words.collect::<Vec<_>>()) {
                Ok(i) => i,
                Err(e) => {
                    eprintln!("gitagent: {e}");
                    continue;
                }
            };
            n += 1;
            let opts = RunOptions {
                instruction: String::new(),
                work_dir: s.work_dir.clone(),
                out_dir: out_dir.join(format!("task-{n}")),
                timeout: s.timeout_set,
                session_id: Some(session_id.clone()),
                events: s.events.clone(),
                resume: None,
                sandbox: s.sandbox.clone(),
                tool_allowlist: None,
                tool_denylist: Vec::new(),
                hook_callbacks: None,
                mcp: mcp.clone(),
                approver: Some(crate::approver()),
                kill: None,
                steer: None,
                external_tools: None,
                throttle: None,
            };
            match gitagent_core::run_workflow(&s.spec, s.provider.as_ref(), &name, &inputs, opts)
                .await
            {
                Ok(report) => println!("{}", report.text()),
                Err(e) => eprintln!("gitagent: {e:#}"),
            }
            continue;
        }
        if matches!(line, "/new" | "/reset") {
            if resume.take().is_some() {
                eprintln!(
                    "{}",
                    st.dim("fresh conversation; the shell and history were dropped")
                );
            } else {
                eprintln!("{}", st.dim("already a fresh conversation"));
            }
            continue;
        }
        if line == "/clear" {
            if st_is_color(&st) {
                eprint!("\x1b[2J\x1b[H");
            }
            continue;
        }
        let task = match line.strip_prefix('/') {
            Some(cmd) => match command(&s, cmd) {
                Some(prompt) => prompt,
                None => continue,
            },
            None => line.to_string(),
        };
        n += 1;
        let started = std::time::Instant::now();
        let opts = RunOptions {
            instruction: task,
            work_dir: s.work_dir.clone(),
            out_dir: out_dir.join(format!("task-{n}")),
            timeout: s.timeout_set,
            session_id: Some(session_id.clone()),
            events: events.clone(),
            resume: resume.take(),
            sandbox: s.sandbox.clone(),
            tool_allowlist: None,
            tool_denylist: Vec::new(),
            hook_callbacks: None,
            mcp: mcp.clone(),
            approver: Some(crate::approver()),
            kill: None,
            steer: None,
            external_tools: None,
            throttle: None,
        };
        let spinner = console::Spinner::start(st, "thinking");
        tokio::select! {
            result = gitagent_core::run(&s.spec, s.provider.as_ref(), opts) => match result {
                Ok(finished) => {
                    drop(spinner);
                    let r = &finished.report;
                    match &r.summary {
                        Some(text) => console::reply(&out_st, &agent_name, text),
                        None => console::reply(&out_st, &agent_name, &format!("(no reply: the run ended with {})", r.outcome)),
                    }
                    console::status(&st, &r.outcome.to_string(), r.turns, started.elapsed(), r.usage.input_tokens, r.usage.output_tokens);
                    eprintln!();
                    crate::export_telemetry(&finished, &s.spec).await;
                    crate::learn_cmd::after_run(&s, &finished.report, &s.work_dir, false).await;
                    resume = Some(finished.into_resume());
                }
                Err(e) => eprintln!("gitagent: {e:#}"),
            },
            _ = tokio::signal::ctrl_c() => {
                // The run, its shell and its history were dropped with it.
                eprintln!("\ngitagent: stopped; the next message starts a fresh conversation");
            }
        }
    }
    if let Some(r) = resume.take() {
        let opts = RunOptions {
            instruction: String::new(),
            work_dir: s.work_dir.clone(),
            out_dir: out_dir.join("teardown"),
            timeout: s.timeout_set,
            session_id: Some(session_id.clone()),
            events: s.events.clone(),
            resume: Some(r),
            sandbox: s.sandbox.clone(),
            tool_allowlist: None,
            tool_denylist: Vec::new(),
            hook_callbacks: None,
            mcp: mcp.clone(),
            approver: Some(crate::approver()),
            kill: None,
            steer: None,
            external_tools: None,
            throttle: None,
        };
        match gitagent_core::teardown(&s.spec, s.provider.as_ref(), opts).await {
            Some(Ok(done)) => eprintln!(
                "gitagent: teardown: {}",
                done.report.summary.as_deref().unwrap_or("(no reply)")
            ),
            Some(Err(e)) => eprintln!("gitagent: teardown failed: {e:#}"),
            None => {}
        }
    }
    gitagent_core::end_session(&s.spec, &s.work_dir, &session_id).await;
    Ok(())
}

/// What the session is, on stderr.
fn banner(s: &Setup, st: &Style) {
    let spec = &s.spec;
    let (name, version) = match &spec.agent {
        Some(a) => (a.manifest.name.clone(), a.manifest.version.clone()),
        None => (
            "gitagent".to_string(),
            env!("CARGO_PKG_VERSION").to_string(),
        ),
    };
    let branch = std::process::Command::new("git")
        .current_dir(&s.work_dir)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    let home = std::env::var("HOME").unwrap_or_default();
    let dir = s.work_dir.display().to_string();
    let dir = match dir.strip_prefix(&home) {
        Some(rest) if !home.is_empty() => format!("~{rest}"),
        _ => dir,
    };
    let mut extra = vec![format!("tools: {}", tool_names(s).join(", "))];
    if !spec.skills.is_empty() {
        let names: Vec<&str> = spec.skills.iter().map(|k| k.name.as_str()).collect();
        extra.push(format!("skills: {}", names.join(", ")));
    }
    if spec.profile.tools.subagents && !spec.agents.is_empty() {
        let names: Vec<&str> = spec.agents.iter().map(|a| a.name.as_str()).collect();
        extra.push(format!("agents: {}", names.join(", ")));
    }
    if let Some(e) = &spec.env_config.name {
        extra.push(format!("env: {e}"));
    }
    console::banner(
        st,
        &name,
        &version,
        &spec.model.to_string(),
        &branch,
        &dir,
        &extra,
    );
}

/// `/help`: a guide to the session, printed to stdout.
fn help(s: &Setup) {
    let st = Style::for_stdout();
    let spec = &s.spec;
    let h = |t: &str| println!("\n{}", st.bold(&st.orange(t)));
    let row = |c: &str, d: &str| {
        let pad = " ".repeat(24usize.saturating_sub(c.chars().count()).max(2));
        println!("  {}{pad}{}", st.bold(c), st.dim(d));
    };
    println!(
        "{} {}",
        st.bold(&st.orange(&format!("gitagent v{}", env!("CARGO_PKG_VERSION")))),
        st.dim("· how this session works")
    );

    h("Commands");
    for (c, d) in COMMANDS {
        row(c, d);
    }
    row(
        "@<workflow> k=v …",
        "run a workflow step by step, without the model choosing",
    );
    row("/", "this guide");

    h("Talking to the agent");
    println!("  Type a message and press Enter; each message continues the same conversation.");
    println!(
        "  {} is a tool call, the dim line under it the result; {} marks an error or a blocked action;",
        st.orange("$"),
        st.orange("✗")
    );
    println!(
        "  {} closes the turn with the outcome, time and tokens. Ctrl-C stops a running turn.",
        st.orange("●")
    );

    h("Skills and workflows");
    if spec.skills.is_empty() {
        println!("  No skills in skills/. A skill is a SKILL.md with instructions the agent follows when asked.");
    } else {
        for k in &spec.skills {
            row(&format!("/skill:{}", k.name), &k.description);
        }
    }
    if !spec.workflows.is_empty() {
        for w in &spec.workflows {
            row(&format!("@{}", w.name), &w.description);
        }
    }

    h("Approvals");
    println!(
        "  When a rule needs a person, the session pauses with {} and waits for y or n.",
        st.orange("! approval needed")
    );
    println!("  Rules live in agent.yaml under compliance, and in hooks/ scripts.");

    h("Keys");
    row("Tab", "complete the command or workflow under the cursor");
    row(
        "↑ ↓",
        "earlier messages, or a pick while the menu is showing",
    );
    row("Ctrl-A / Ctrl-E", "start / end of the line");
    row("Ctrl-U / Ctrl-W", "clear the line / delete the last word");
    row(
        "Ctrl-C",
        "clear the line; on an empty line, end the session",
    );
    row("Ctrl-D", "end the session");

    h("Where the agent lives");
    let root = spec
        .agent
        .as_ref()
        .map(|a| a.root.display().to_string())
        .unwrap_or_else(|| s.work_dir.display().to_string());
    println!("  {}", st.dim(&root));
    for (f, d) in [
        ("agent.yaml", "name, model, tools, compliance, plugins"),
        (
            "SOUL.md · RULES.md",
            "who the agent is · what it must never do",
        ),
        ("memory/MEMORY.md", "what it remembers between sessions"),
        (
            "skills/ · tools/",
            "instructions it can follow · YAML tools it can call",
        ),
        (
            "hooks/hooks.yaml",
            "scripts that run before and after tool calls",
        ),
        (
            "schedules/ · workflows/",
            "prompts on a timetable · step-by-step procedures",
        ),
        (
            "knowledge/ · agents/",
            "documents it can read · sub-agents it can delegate to",
        ),
    ] {
        row(f, d);
    }

    h("Outside the session");
    row("gitagent -p \"task\" -d dir", "one task, then exit");
    row("gitagent schedule …", "list, add, run, start the schedules");
    row(
        "gitagent plugin …",
        "install, list, enable, disable plugins",
    );
    row(
        "gitagent validate dir",
        "check the agent's files (--compliance for the rules)",
    );
    row("gitagent --help", "every flag");
    println!();
}

/// What the line editor can complete: the commands and the workflows.
fn choices(s: &Setup) -> Vec<lineedit::Choice> {
    let mut v: Vec<lineedit::Choice> = COMMANDS
        .iter()
        .map(|(c, d)| lineedit::Choice {
            text: c.trim_end_matches(" …").to_string(),
            about: d.to_string(),
        })
        .collect();
    for k in &s.spec.skills {
        v.push(lineedit::Choice {
            text: format!("/skill:{}", k.name),
            about: k.description.clone(),
        });
    }
    for w in &s.spec.workflows {
        v.push(lineedit::Choice {
            text: format!("@{}", w.name),
            about: w.description.clone(),
        });
    }
    v
}

/// The commands and one line about each, for /help and the editor.
const COMMANDS: [(&str, &str); 11] = [
    ("/memory", "show the working memory"),
    ("/skills", "list the skills"),
    ("/agents", "list the sub-agents"),
    ("/tools", "list the tools"),
    ("/plugins", "list the plugins"),
    (
        "/schedules",
        "list the schedules (run them with `gitagent schedule start`)",
    ),
    (
        "/workflows",
        "list the workflows (run one with @<name> key=value …)",
    ),
    (
        "/skill:<name> …",
        "send a skill's instructions with your request",
    ),
    ("/new", "start a fresh conversation (keeps the session)"),
    ("/clear", "clear the screen"),
    ("/quit", "end the session"),
];

fn st_is_color(st: &Style) -> bool {
    st.is_color()
}

fn tool_names(s: &Setup) -> Vec<String> {
    gitagent_core::offered_tools(&s.spec, None, None, None, &[])
        .into_iter()
        .map(|t| t.name)
        .collect()
}

/// A `/command`: prints its answer and gives back nothing, or gives the
/// message to send (`/skill:<name> [request]`).
fn command(s: &Setup, cmd: &str) -> Option<String> {
    let spec = &s.spec;
    let (name, rest) = cmd.split_once(' ').unwrap_or((cmd, ""));
    match name {
        "help" => help(s),
        "memory" => match &spec.memory {
            None => println!("(memory is off)"),
            Some(m) => match std::fs::read_to_string(m.dir.join(&m.working.path)) {
                Err(_) => println!("(no memory file)"),
                Ok(t) if t.trim().is_empty() => println!("(empty)"),
                Ok(t) => println!("--- memory ---\n{}\n--- end ---", t.trim_end()),
            },
        },
        "skills" => {
            if spec.skills.is_empty() {
                println!("No skills.");
            }
            for k in &spec.skills {
                println!("{} — {}", k.name, k.description);
            }
        }
        "agents" => {
            if !spec.profile.tools.subagents || spec.agents.is_empty() {
                println!("No sub-agents.");
            } else {
                for a in &spec.agents {
                    println!("{} — {}", a.name, a.description);
                }
            }
        }
        "tools" => println!("{}", tool_names(s).join(", ")),
        "plugins" => {
            if spec.plugins.is_empty() {
                println!("No plugins.");
            }
            for p in &spec.plugins {
                println!("{} v{} — {}", p.id, p.version, p.description);
            }
        }
        "tasks" | "learned" => println!(
            "/{name} belonged to 1.x's skill learner, which this version does not have; `learning: online` in gitagent.yaml learns lessons instead"
        ),
        "workflows" => {
            if spec.workflows.is_empty() {
                println!("No workflows.");
            }
            for w in &spec.workflows {
                println!("{} — {}", w.name, w.description);
            }
        }
        "schedules" => {
            if spec.schedules.is_empty() {
                println!("No schedules (gitagent schedule add … creates one).");
            }
            for sch in &spec.schedules {
                println!(
                    "{} — {}{} — {}",
                    sch.id,
                    sch.when(),
                    if sch.enabled { "" } else { " (disabled)" },
                    sch.description
                );
            }
        }
        _ => {
            if let Some(skill_name) = name.strip_prefix("skill:") {
                let Some(skill) = spec.skills.iter().find(|k| k.name == skill_name) else {
                    eprintln!("Unknown skill: {skill_name}");
                    return None;
                };
                let text = std::fs::read_to_string(&skill.path).unwrap_or_default();
                let body = gitagent_loader::split_frontmatter(&text)
                    .1
                    .trim()
                    .to_string();
                return Some(if rest.trim().is_empty() {
                    format!("Follow the skill `{skill_name}`:\n\n{body}")
                } else {
                    format!(
                        "Use the skill `{skill_name}` for this request:\n\n{body}\n\n# Request\n\n{}",
                        rest.trim()
                    )
                });
            }
            eprintln!("Unknown command /{name}; /help lists them");
        }
    }
    None
}
