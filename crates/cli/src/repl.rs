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

use crate::{report, Setup};

pub(crate) async fn run(s: Setup, out_dir: &Path) -> Result<()> {
    banner(&s);
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut resume: Option<Resume> = None;
    let mut n = 0;
    let session_id = format!("gitagent-{}", std::process::id());
    let mcp = crate::connect_mcp(&s.spec).await;
    loop {
        eprint!("→ ");
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
        let opts = RunOptions {
            instruction: task,
            work_dir: s.work_dir.clone(),
            out_dir: out_dir.join(format!("task-{n}")),
            timeout: Some(s.timeout),
            session_id: Some(session_id.clone()),
            events: s.events.clone(),
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
                    report(&finished, !s.events.enabled());
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
fn banner(s: &Setup) {
    let spec = &s.spec;
    match &spec.agent {
        Some(a) => eprintln!("{} v{}", a.manifest.name, a.manifest.version),
        None => eprintln!("gitagent {}", env!("CARGO_PKG_VERSION")),
    }
    eprintln!("Model: {}", spec.model);
    eprintln!("Tools: {}", tool_names(s).join(", "));
    if !spec.skills.is_empty() {
        let names: Vec<&str> = spec.skills.iter().map(|k| k.name.as_str()).collect();
        eprintln!("Skills: {}", names.join(", "));
    }
    if spec.profile.tools.subagents && !spec.agents.is_empty() {
        let names: Vec<&str> = spec.agents.iter().map(|a| a.name.as_str()).collect();
        eprintln!("Agents: {}", names.join(", "));
    }
    eprintln!("Type /help for commands, /quit to exit. Ctrl-C stops the current turn.");
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
        "help" => println!(
            "/memory           show the working memory\n\
             /skills           list the skills\n\
             /agents           list the sub-agents\n\
             /tools            list the tools\n\
             /plugins          list the plugins\n\
             /schedules        list the schedules (run them with `gitagent schedule start`)\n\
             /workflows        list the workflows (run one with @<name> key=value …)\n\
             /skill:<name> …   send a skill's instructions with your request\n\
             /quit             end the session"
        ),
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
