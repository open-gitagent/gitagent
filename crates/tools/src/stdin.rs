use anyhow::Result;
use gitagent_provider::ToolCall;
use gitagent_terminal::decode_keys;

use crate::exec::{capped_yield, finish};
use crate::{int_arg, ArgError, ToolContext, ToolOutput, STDIN_YIELD_MS};

pub(crate) async fn run(call: &ToolCall, ctx: &mut ToolContext<'_>) -> Result<ToolOutput> {
    let id = int_arg(call, "session_id")?
        .ok_or_else(|| ArgError("missing required argument `session_id`".into()))?;
    let id = u32::try_from(id).map_err(|_| ArgError("`session_id` is too large".into()))?;
    let chars = match call.arguments.get("chars") {
        None | Some(serde_json::Value::Null) => "",
        Some(serde_json::Value::String(s)) => s.as_str(),
        Some(_) => return Err(ArgError("`chars` must be a string".into()).into()),
    };
    let keys = decode_keys(chars);
    let default_wait = if keys.is_empty() {
        ctx.default_yield_ms
    } else {
        STDIN_YIELD_MS
    };
    let yield_ms = capped_yield(int_arg(call, "yield_ms")?.unwrap_or(default_wait), ctx);

    if keys.is_empty() {
        ctx.counters.inc("stdin_polls");
    } else {
        ctx.counters.inc("stdin_writes");
    }
    if keys.contains(&0x03) {
        ctx.counters.inc("interrupts");
    }
    let result = ctx.terminal.write_stdin(id, &keys, yield_ms).await?;
    Ok(finish(&result, ctx))
}
