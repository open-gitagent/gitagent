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
use crate::Setup;

pub(crate) async fn run(s: Setup, out_dir: &Path) -> Result<()> {
    let st = Style::for_stderr();
    let out_st = Style::for_stdout();
    banner(&s, &st);
    let agent_name = s
        .spec
        .agent
        .as_ref()
        .map_or_else(|| s.spec.config.name.clone(), |a| a.manifest.name.clone());
    // Live lines for each tool call and result, on stderr.
    let events = s.events.clone().also(move |e| {
        if let Some(line) = console::event_line(&st, &e) {
            eprintln!("{line}");
        }
    });
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut resume: Option<Resume> = None;
    let mut n = 0;
    let session_id = format!("gitagent-{}", std::process::id());
    let mcp = crate::connect_mcp(&s.spec).await;
    loop {
        eprint!("{}", console::prompt(&st));
        let _ = std::io::stderr().flush();
        let Some(line) = lines.next_line().await? else {
            eprintln!();
            break;
        };
        let line = line.trim();
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
                timeout: Some(s.timeout),
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
            };
            match gitagent_core::run_workflow(&s.spec, s.provider.as_ref(), &name, &inputs, opts)
                .await
            {
                Ok(report) => println!("{}", report.text()),
                Err(e) => eprintln!("gitagent: {e:#}"),
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
            timeout: Some(s.timeout),
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
        };
        tokio::select! {
            result = gitagent_core::run(&s.spec, s.provider.as_ref(), opts) => match result {
                Ok(finished) => {
                    let r = &finished.report;
                    match &r.summary {
                        Some(text) => console::reply(&out_st, &agent_name, text),
                        None => console::reply(&out_st, &agent_name, &format!("(no reply: the run ended with {})", r.outcome)),
                    }
                    console::status(&st, &r.outcome.to_string(), r.turns, started.elapsed(), r.usage.input_tokens, r.usage.output_tokens);
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
            timeout: Some(s.timeout),
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
        &extra,
    );
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
        "help" => {
            let st = Style::for_stdout();
            for (c, d) in [
                ("/memory", "show the working memory"),
                ("/skills", "list the skills"),
                ("/agents", "list the sub-agents"),
                ("/tools", "list the tools"),
                ("/plugins", "list the plugins"),
                ("/schedules", "list the schedules (run them with `gitagent schedule start`)"),
                ("/workflows", "list the workflows (run one with @<name> key=value …)"),
                ("/skill:<name> …", "send a skill's instructions with your request"),
                ("/quit", "end the session"),
            ] {
                let pad = " ".repeat(18usize.saturating_sub(c.chars().count()));
                println!("{}{pad}{}", st.bold(c), st.dim(d));
            }
        }
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
