//! Approval at the terminal: the person running gitagent is asked on
//! `/dev/tty`, so it works while stdout carries events. With no terminal the
//! request is denied.

use std::io::{BufRead, Write};

use gitagent_core::{Approval, ApprovalRequest, Approver};

pub(crate) struct TtyApprover;

#[async_trait::async_trait]
impl Approver for TtyApprover {
    async fn approve(&self, request: &ApprovalRequest) -> Approval {
        let question = format!(
            "\ngitagent: approval needed ({})\n  {} {}\nApprove? [y/N] ",
            request.reason,
            request.tool_name,
            serde_json::Value::Object(request.arguments.clone())
        );
        let asked = tokio::task::spawn_blocking(move || -> std::io::Result<String> {
            let mut tty = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/tty")?;
            tty.write_all(question.as_bytes())?;
            tty.flush()?;
            let mut line = String::new();
            std::io::BufReader::new(tty).read_line(&mut line)?;
            Ok(line)
        })
        .await;
        match asked {
            Ok(Ok(line)) if matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes") => {
                Approval {
                    approved: true,
                    reason: None,
                }
            }
            Ok(Ok(_)) => Approval::denied("denied at the terminal"),
            _ => Approval::denied("there is no terminal to ask on"),
        }
    }
}
