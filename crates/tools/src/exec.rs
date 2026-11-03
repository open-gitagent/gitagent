use anyhow::Result;
use gitagent_provider::ToolCall;
use gitagent_shaper::Shaped;
use gitagent_terminal::{ExecResult, MAIN_SESSION};

use gitagent_context::MASK_PREFIX;

use crate::{bool_arg, int_arg, str_arg, ToolContext, ToolOutput, MAX_YIELD_MS};

pub(crate) async fn run(call: &ToolCall, ctx: &mut ToolContext<'_>) -> Result<ToolOutput> {
    let cmd = str_arg(call, "cmd")?;
    let background = ctx.tools.write_stdin && bool_arg(call, "background")?;
    let yield_ms = capped_yield(
        int_arg(call, "yield_ms")?.unwrap_or(ctx.default_yield_ms),
        ctx,
    );
    if ctx.shaper.mentions_spill(cmd) {
        ctx.counters.inc("spill_reads");
    }
    let result = ctx.terminal.exec(cmd, yield_ms, background).await?;
    if background {
        ctx.counters.inc("background_sessions");
    }
    Ok(finish(&result, ctx))
}

pub(crate) fn capped_yield(yield_ms: u64, ctx: &ToolContext<'_>) -> u64 {
    let mut ms = yield_ms.min(MAX_YIELD_MS);
    if let Some(left) = ctx.remaining {
        ms = ms.min(left.as_millis() as u64);
    }
    ms
}

/// Shapes a terminal result, records its counters and formats it for the model.
pub(crate) fn finish(r: &ExecResult, ctx: &mut ToolContext<'_>) -> ToolOutput {
    if !r.finished {
        ctx.counters.inc("exec_timeouts");
    }
    if r.shell_restarted {
        ctx.counters.inc("shell_restarts");
    }
    let shaped = ctx.shaper.shape(&r.output);
    ctx.counters.add("shaped_bytes_in", r.output.len() as u64);
    ctx.counters
        .add("shaped_bytes_out", shaped.text.len() as u64);
    ctx.counters
        .add("collapse_events", shaped.collapsed_runs as u64);
    let mut out = ToolOutput::ok(format_result(
        r,
        &shaped,
        ctx.tools.write_stdin,
        ctx.minutes_left,
    ));
    out.finished = r.finished;
    let status = match (r.finished, r.exit) {
        (true, Some(code)) => format!("exit {code}"),
        (true, None) => "shell exited".to_string(),
        (false, _) => "still running".to_string(),
    };
    out.stub = Some(match &shaped.spill_path {
        Some(p) => format!(
            "{MASK_PREFIX}; status was {status}, {} lines; full output: {}]",
            shaped.total_lines,
            p.display()
        ),
        None => format!("{MASK_PREFIX}; status was {status}]"),
    });
    out
}

pub(crate) fn format_result(
    r: &ExecResult,
    shaped: &Shaped,
    sessions: bool,
    minutes_left: Option<u64>,
) -> String {
    let mut out = shaped.text.clone();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    let secs = r.elapsed.as_secs_f64();
    let label = if r.session > MAIN_SESSION {
        format!("session {} · ", r.session)
    } else {
        String::new()
    };
    // Point at the full output only when the model is not seeing all of it.
    let mut full = match &shaped.spill_path {
        Some(p) if shaped.hidden_lines > 0 || r.omitted_bytes > 0 => format!(
            " · {} lines, full output: {}",
            shaped.total_lines,
            p.display()
        ),
        _ => String::new(),
    };
    if let Some(m) = minutes_left {
        full.push_str(&format!(" · {m} min left"));
    }
    let status = match (r.finished, r.exit) {
        (true, Some(code)) => format!("[{label}exit {code} · {secs:.1}s{full}]"),
        (true, None) => format!("[{label}shell exited · {secs:.1}s{full}]"),
        (false, _) if sessions => {
            let id = r.session.max(MAIN_SESSION);
            format!(
                "[session {id} still running after {secs:.1}s{full} · output so far is shown · use write_stdin with session_id {id} to wait, send input or interrupt]"
            )
        }
        (false, _) => format!(
            "[still running after {secs:.1}s{full} · output so far is shown · the next exec call interrupts it]"
        ),
    };
    out.push_str(&status);
    for note in &r.notes {
        out.push('\n');
        out.push_str(note);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use gitagent_shaper::{shape, ShapeCfg};
    use std::time::Duration;

    fn result(output: &str, exit: Option<i32>, finished: bool, session: u32) -> ExecResult {
        ExecResult {
            session,
            output: output.as_bytes().to_vec(),
            exit,
            finished,
            omitted_bytes: 0,
            elapsed: Duration::from_millis(1500),
            shell_restarted: false,
            notes: vec![],
        }
    }

    fn fmt(r: &ExecResult, sessions: bool) -> String {
        let dir = tempfile::tempdir().unwrap();
        let cfg = ShapeCfg {
            cap_tokens: 8000,
            head_ratio: 0.3,
            collapse_min_run: 3,
        };
        let shaped = shape(&r.output, &cfg, dir.path(), 1);
        format_result(r, &shaped, sessions, None)
            .replace(&dir.path().display().to_string(), "SPILL")
    }

    #[test]
    fn formats_finished_and_running_commands() {
        assert_eq!(
            fmt(&result("hi\r\n", Some(0), true, 1), true),
            "hi\n[exit 0 · 1.5s]"
        );
        assert_eq!(
            fmt(&result("", Some(2), true, 3), true),
            "[session 3 · exit 2 · 1.5s]"
        );
        assert!(fmt(&result("part", None, false, 2), true)
            .starts_with("part\n[session 2 still running after 1.5s"));
        assert!(fmt(&result("part", None, false, 1), false)
            .ends_with("the next exec call interrupts it]"));
    }

    #[test]
    fn names_the_spill_file_when_lines_are_hidden() {
        let many: String = (0..20).map(|i| format!("warning: item {i}\n")).collect();
        let out = fmt(&result(&many, Some(0), true, 1), true);
        assert!(out.contains("[18 similar lines hidden]"));
        assert!(
            out.ends_with("[exit 0 · 1.5s · 20 lines, full output: SPILL/1.log]"),
            "{out}"
        );
    }
}
