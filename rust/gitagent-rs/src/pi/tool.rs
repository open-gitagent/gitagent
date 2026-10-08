//! The tool abstraction. `execution_mode` is the field the loop actually reads
//! to decide parallel vs sequential (faithful to pi-agent-core).

use crate::pi::message::ToolResult;
use async_trait::async_trait;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionMode {
    Parallel,
    Sequential,
}

#[async_trait]
pub trait AgentTool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    /// JSON Schema for arguments.
    fn parameters(&self) -> Value;
    /// Whether this tool may run concurrently with others in the same turn.
    /// Anything that writes files / runs git / has side effects → Sequential.
    fn execution_mode(&self) -> ExecutionMode {
        ExecutionMode::Parallel
    }
    /// Whether the tool changes state (blocked in `plan` mode unless allowed).
    /// Defaults to "runs sequentially"; tools that run in parallel but can
    /// still mutate (e.g. delegation) override this.
    fn is_mutating(&self) -> bool {
        self.execution_mode() == ExecutionMode::Sequential
    }
    async fn execute(&self, id: &str, args: Value, cancel: &CancellationToken) -> anyhow::Result<ToolResult>;
}
