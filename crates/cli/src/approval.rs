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
        let st = crate::console::Style::for_stderr();
        let question = format!(
            "\n{} {}\n{}  {}\n  {} ",
            st.orange("!"),
            st.bold(&format!(
                "approval needed · {}",
                visible(&request.tool_name)
            )),
            details(&request.arguments),
            st.dim(&visible(&request.reason)),
            st.orange("approve? [y/N]")
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

/// The longest argument shown whole; a longer one keeps its start and end.
const SHOW_CHARS: usize = 4000;

/// Every argument of the call, in full, one per block: what the person is
/// approving, not a one-line summary of it.
fn details(args: &serde_json::Map<String, serde_json::Value>) -> String {
    let mut out = String::new();
    for (key, value) in args {
        let text = match value {
            serde_json::Value::String(s) => s.clone(),
            other => serde_json::to_string_pretty(other).unwrap_or_default(),
        };
        out.push_str(&format!("  {}:\n", visible(key)));
        for line in shorten(&visible(&text)).lines() {
            out.push_str(&format!("    {line}\n"));
        }
        if text.is_empty() {
            out.push_str("    (empty)\n");
        }
    }
    out
}

/// `text` whole, or its start and end with the number of characters left
/// out said plainly, so nothing is hidden without the person knowing.
fn shorten(text: &str) -> String {
    let n = text.chars().count();
    if n <= SHOW_CHARS {
        return text.to_string();
    }
    let head: String = text.chars().take(SHOW_CHARS * 3 / 4).collect();
    let tail: String = text.chars().skip(n - SHOW_CHARS / 4).collect();
    format!(
        "{head}\n[… {} characters not shown; answer n if unsure …]\n{tail}",
        n - SHOW_CHARS
    )
}

/// `text` with everything that could change how the terminal shows it made
/// visible: escape sequences, carriage returns, other control characters
/// and the Unicode marks that reorder text. Newlines and tabs stay.
fn visible(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '\n' | '\t' => c.to_string(),
            '\x1b' => "␛".to_string(),
            '\r' => "␍".to_string(),
            c if c.is_control()
                || matches!(c, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}') =>
            {
                format!("\\u{{{:04x}}}", c as u32)
            }
            c => c.to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prompt_shows_every_argument_in_full_and_makes_controls_visible() {
        let long_cmd = format!("echo {}; rm -rf ~/precious", "x".repeat(200));
        let args = serde_json::json!({"cmd": long_cmd})
            .as_object()
            .unwrap()
            .clone();
        let shown = details(&args);
        assert!(
            shown.contains("rm -rf ~/precious"),
            "nothing past 120 characters may be hidden"
        );

        let args = serde_json::json!({"path": "a.sh", "content": "echo hi\nrm -rf /"})
            .as_object()
            .unwrap()
            .clone();
        let shown = details(&args);
        assert!(
            shown.contains("content:") && shown.contains("rm -rf /"),
            "{shown}"
        );

        assert_eq!(visible("ok\x1b[2K\rsafe"), "ok␛[2K␍safe");
        assert_eq!(visible("a\u{202e}b"), "a\\u{202e}b");

        let huge = "y".repeat(SHOW_CHARS * 2);
        let cut = shorten(&huge);
        assert!(cut.contains(&format!("{} characters not shown", SHOW_CHARS)));
    }
}
