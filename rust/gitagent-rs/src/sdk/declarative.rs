//! Declarative tools — `tools/<name>.yaml` defines a tool whose implementation
//! is a script. The script receives the call arguments as JSON on stdin and its
//! stdout becomes the tool result. Declarative tools default to Sequential (they
//! usually have side effects) so they never race in a parallel batch.

use crate::pi::message::ToolResult;
use crate::pi::tool::{AgentTool, ExecutionMode};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;
use crate::sdk::proc::{new_group, GroupGuard};

#[derive(Deserialize)]
struct DeclSpec {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    parameters: Option<Value>,
    /// Shell command; receives the args JSON on stdin.
    run: String,
    #[serde(default)]
    execution: Option<String>,
}

pub struct DeclarativeTool {
    name: String,
    description: String,
    parameters: Value,
    run: String,
    mode: ExecutionMode,
    cwd: PathBuf,
}

/// Load every `tools/*.yaml` in `dir` as a declarative tool.
pub fn load_declarative_tools(dir: &Path) -> Vec<Arc<dyn AgentTool>> {
    let mut out: Vec<Arc<dyn AgentTool>> = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir.join("tools")) else {
        return out;
    };
    let mut paths: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    paths.sort();
    for p in paths {
        let ext = p.extension().and_then(|e| e.to_str());
        if ext != Some("yaml") && ext != Some("yml") {
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(&p) else { continue };
        let Ok(spec) = serde_yaml::from_str::<DeclSpec>(&raw) else { continue };
        let mode = match spec.execution.as_deref() {
            Some("parallel") => ExecutionMode::Parallel,
            _ => ExecutionMode::Sequential,
        };
        out.push(Arc::new(DeclarativeTool {
            name: spec.name,
            description: spec.description,
            parameters: spec.parameters.unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
            run: spec.run,
            mode,
            cwd: dir.to_path_buf(),
        }));
    }
    out
}

#[async_trait]
impl AgentTool for DeclarativeTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn parameters(&self) -> Value {
        self.parameters.clone()
    }
    fn execution_mode(&self) -> ExecutionMode {
        self.mode
    }
    async fn execute(&self, _id: &str, args: Value, cancel: &CancellationToken) -> anyhow::Result<ToolResult> {
        let payload = args.to_string().into_bytes();
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg(&self.run)
            .current_dir(&self.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        // Own process group so abort/timeout kills the script's children too.
        new_group(&mut cmd);
        let mut child = cmd.spawn()?;
        let mut guard = GroupGuard::new(child.id());
        // Write stdin while draining output — a big payload can't deadlock.
        let stdin = child.stdin.take();
        let write = async move {
            if let Some(mut stdin) = stdin {
                let _ = stdin.write_all(&payload).await;
            }
        };
        let fut = async {
            let (_, out) = tokio::join!(write, child.wait_with_output());
            out
        };
        tokio::pin!(fut);
        let out = tokio::select! {
            _ = cancel.cancelled() => return Ok(ToolResult::text("(aborted)")),
            r = &mut fut => r?,
        };
        guard.disarm();
        let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
        if !out.status.success() {
            s.push_str(&String::from_utf8_lossy(&out.stderr));
            s.push_str(&format!("\n\nExit code: {}", out.status.code().unwrap_or(-1)));
        }
        Ok(ToolResult::text(if s.is_empty() { "(no output)".into() } else { s }))
    }
}
