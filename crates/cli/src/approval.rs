//! Approval at the terminal: the person running gitagent is asked on
//! `/dev/tty`, so it works while stdout carries events. With no terminal the
//! request is denied.
//!
//! One thread reads the terminal for the whole process and hands each line
//! to whoever is asking. A request that times out stops waiting without
//! taking a line with it, and an answer typed after that is thrown away
//! before the next question is shown.

use std::io::{BufRead, Write};
use std::sync::OnceLock;

use gitagent_core::{Approval, ApprovalRequest, Approver};
use tokio::sync::{mpsc, Mutex};

pub(crate) struct TtyApprover;

static LINES: OnceLock<Mutex<mpsc::UnboundedReceiver<String>>> = OnceLock::new();

/// The terminal's lines, started on first use; `None` without a terminal.
fn lines() -> Option<&'static Mutex<mpsc::UnboundedReceiver<String>>> {
    let tty = std::fs::File::open("/dev/tty").ok()?;
    Some(LINES.get_or_init(|| {
        let (tx, rx) = mpsc::unbounded_channel();
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(tty).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Mutex::new(rx)
    }))
}

#[async_trait::async_trait]
impl Approver for TtyApprover {
    async fn approve(&self, request: &ApprovalRequest) -> Approval {
        let Some(lines) = lines() else {
            return Approval::denied("there is no terminal to ask on");
        };
        let mut rx = lines.lock().await;
        // Answers to questions nobody is waiting for any more.
        while rx.try_recv().is_ok() {}
        let question = format!(
            "\ngitagent: approval needed ({})\n  {} {}\nApprove? [y/N] ",
            request.reason,
            request.tool_name,
            serde_json::Value::Object(request.arguments.clone())
        );
        let shown = std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/tty")
            .and_then(|mut tty| {
                tty.write_all(question.as_bytes())
                    .and_then(|()| tty.flush())
            });
        if shown.is_err() {
            return Approval::denied("there is no terminal to ask on");
        }
        match rx.recv().await {
            Some(line) if matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes") => {
                Approval {
                    approved: true,
                    reason: None,
                }
            }
            Some(_) => Approval::denied("denied at the terminal"),
            None => Approval::denied("the terminal was closed"),
        }
    }
}
