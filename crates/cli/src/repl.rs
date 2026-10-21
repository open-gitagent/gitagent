//! Interactive mode: tasks are read from stdin one line at a time. Each task
//! continues the same conversation and shell. Ctrl-C stops the current task
//! (and starts a fresh conversation); Ctrl-D or `exit` ends the session.

use std::io::Write;
use std::path::Path;

use anyhow::Result;
use gitagent_core::{Resume, RunOptions};
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::{report, Setup};

pub(crate) async fn run(s: Setup, out_dir: &Path) -> Result<()> {
    eprintln!(
        "Type a task and press Enter. Ctrl-C stops the current task; Ctrl-D or `exit` quits."
    );
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut resume: Option<Resume> = None;
    let mut n = 0;
    let session_id = format!("gitagent-{}", std::process::id());
    let mcp = crate::connect_mcp(&s.spec).await;
    loop {
        eprint!("> ");
        let _ = std::io::stderr().flush();
        let Some(line) = lines.next_line().await? else {
            eprintln!();
            break;
        };
        let task = line.trim();
        if task.is_empty() {
            continue;
        }
        if matches!(task, "exit" | "quit") {
            break;
        }
        n += 1;
        let opts = RunOptions {
            instruction: task.to_string(),
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
                eprintln!("\ngitagent: task stopped; the next task starts a fresh conversation");
            }
        }
    }
    gitagent_core::end_session(&s.spec, &s.work_dir, &session_id).await;
    Ok(())
}
