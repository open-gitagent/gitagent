//! The four builtin tools as `AgentTool` implementations. Mutating tools return
//! `ExecutionMode::Sequential` so they never race in a parallel batch.

use crate::pi::agent::Agent;
use crate::pi::event::{AgentEvent, DeltaKind};
use crate::pi::gate::ToolGate;
use crate::pi::message::ToolResult;
use crate::pi::provider::GenParams;
use crate::pi::tool::{AgentTool, ExecutionMode};
use crate::sdk::declarative::load_declarative_tools;
use crate::sdk::learning::{SkillLearner, TaskTracker};
use crate::sdk::loader::{discover_sub_agents, load_agent};
use crate::sdk::mcp::load_mcp_tools;
use crate::sdk::manifest::PermissionConfig;
use crate::sdk::pdf::PdfTool;
use crate::sdk::permissions::PermissionGate;
use crate::sdk::proc::{new_group, GroupGuard};
use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::process::Stdio;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

/// Safety policy a parent run hands to its `run_agent` sub-agents: the parent's
/// gates (permission rules, hooks) and its permission-mode override.
#[derive(Clone, Default)]
pub struct DelegationPolicy {
    /// Filled in by the parent once its gates exist (they are built after the
    /// tool list). Applied to every sub-agent tool call.
    pub gates: Arc<RwLock<Vec<Arc<dyn ToolGate>>>>,
    /// The parent's permission mode override (plan | acceptEdits | …).
    pub permission_mode: Option<String>,
}

/// How [`builtin_tools_with`] assembles the tool list.
#[derive(Clone, Default)]
pub struct ToolOptions {
    /// Skip agent-supplied executables (`tools/*.yaml`, `mcp.yaml`) — for
    /// agents cloned from a repository the user hasn't marked as trusted.
    pub skip_agent_executables: bool,
    pub delegation: DelegationPolicy,
}

/// The full builtin tool set for a (trusted, local) agent directory.
pub fn builtin_tools(dir: &Path) -> Vec<Arc<dyn AgentTool>> {
    builtin_tools_with(dir, &ToolOptions::default())
}

pub fn builtin_tools_with(dir: &Path, opts: &ToolOptions) -> Vec<Arc<dyn AgentTool>> {
    let mut tools: Vec<Arc<dyn AgentTool>> = vec![
        Arc::new(Cli { cwd: dir.to_path_buf(), sandbox: load_sandbox_wrapper(dir) }),
        Arc::new(Read { cwd: dir.to_path_buf() }),
        Arc::new(WriteTool { cwd: dir.to_path_buf() }),
        Arc::new(EditTool { cwd: dir.to_path_buf() }),
        Arc::new(Memory { dir: dir.to_path_buf() }),
        Arc::new(PdfTool { cwd: dir.to_path_buf() }),
        Arc::new(TaskTracker { dir: dir.to_path_buf() }),
        Arc::new(SkillLearner { dir: dir.to_path_buf() }),
    ];
    // Delegation: expose run_agent only when the agent actually has sub-agents.
    let subs = discover_sub_agents(dir);
    if !subs.is_empty() {
        tools.push(Arc::new(RunAgent {
            base_dir: dir.to_path_buf(),
            sub_agents: subs,
            policy: opts.delegation.clone(),
        }));
    }
    if opts.skip_agent_executables {
        return tools;
    }
    // Declarative tools (tools/*.yaml) — script-backed, discovered per directory.
    tools.extend(load_declarative_tools(dir));
    // MCP tools (mcp.yaml) — external servers over stdio, namespaced mcp__*.
    tools.extend(load_mcp_tools(dir));
    tools
}

/// Resolve a tool path against the agent dir. Relative paths may not climb out
/// of it with `..` (absolute paths are taken as given — permission rules are
/// what restrict those).
fn resolve(cwd: &Path, p: &str) -> anyhow::Result<PathBuf> {
    let path = Path::new(p);
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    match crate::sdk::permissions::normalize_relative(path) {
        Some(rel) => Ok(cwd.join(rel)),
        None => anyhow::bail!("path '{p}' escapes the agent directory"),
    }
}

/// Largest file the `read` tool loads (documents are extracted from memory).
const MAX_READ_BYTES: u64 = 32 * 1024 * 1024;

/// Read `sandbox.wrapper` from agent.yaml (best-effort; None if unset/missing).
fn load_sandbox_wrapper(dir: &Path) -> Option<Vec<String>> {
    let raw = std::fs::read_to_string(dir.join("agent.yaml")).ok()?;
    let manifest: crate::sdk::manifest::AgentManifest = serde_yaml::from_str(&raw).ok()?;
    let wrapper = manifest.sandbox?.wrapper;
    (!wrapper.is_empty()).then_some(wrapper)
}

fn tail(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let t: String = s.chars().rev().take(max).collect::<Vec<_>>().into_iter().rev().collect();
    format!("[truncated, showing last ~{max} chars]\n{t}")
}

// ── cli ─────────────────────────────────────────────────────────────────
pub struct Cli {
    pub cwd: PathBuf,
    /// Optional sandbox argv prefix (`{cwd}` substituted); the command is the
    /// final argument. `None` = run directly via `sh -c`.
    pub sandbox: Option<Vec<String>>,
}

impl Cli {
    /// Build the process to run: sandbox wrapper + command, else `sh -c command`.
    fn command_for(&self, command: &str) -> Command {
        match &self.sandbox {
            Some(wrapper) if !wrapper.is_empty() => {
                let cwd = self.cwd.display().to_string();
                let mut it = wrapper.iter().map(|tok| tok.replace("{cwd}", &cwd));
                let mut cmd = Command::new(it.next().unwrap());
                for arg in it {
                    cmd.arg(arg);
                }
                cmd.arg(command); // command is a single argv element — not interpolated
                cmd
            }
            _ => {
                let mut cmd = Command::new("sh");
                cmd.arg("-c").arg(command);
                cmd
            }
        }
    }
}

#[async_trait]
impl AgentTool for Cli {
    fn name(&self) -> &str { "cli" }
    fn description(&self) -> &str { "Execute a shell command; returns combined stdout/stderr." }
    fn parameters(&self) -> Value {
        json!({ "type": "object", "properties": { "command": { "type": "string" } }, "required": ["command"] })
    }
    fn execution_mode(&self) -> ExecutionMode { ExecutionMode::Sequential }
    async fn execute(&self, _id: &str, args: Value, cancel: &CancellationToken) -> anyhow::Result<ToolResult> {
        let command = args.get("command").and_then(Value::as_str).unwrap_or_default().to_string();
        let mut cmd = self.command_for(&command);
        cmd.current_dir(&self.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        // Own process group: abort/timeout (this future being dropped) kills
        // the whole tree, not just `sh`.
        new_group(&mut cmd);
        let child = cmd.spawn()?;
        let mut guard = GroupGuard::new(child.id());
        let fut = child.wait_with_output();
        tokio::pin!(fut);
        let out = tokio::select! {
            _ = cancel.cancelled() => return Ok(ToolResult::text("(aborted)")),
            r = &mut fut => r?,
        };
        guard.disarm();
        let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
        s.push_str(&String::from_utf8_lossy(&out.stderr));
        s = tail(&s, 100_000);
        if !out.status.success() {
            s.push_str(&format!("\n\nExit code: {}", out.status.code().unwrap_or(-1)));
        }
        Ok(ToolResult::text(if s.is_empty() { "(no output)".into() } else { s }))
    }
}

// ── read ────────────────────────────────────────────────────────────────
pub struct Read {
    pub cwd: PathBuf,
}

#[async_trait]
impl AgentTool for Read {
    fn name(&self) -> &str { "read" }
    fn description(&self) -> &str {
        "Read a file. Text is returned as-is; documents (.docx, .xlsx, .pptx, .pdf, .doc, .rtf, .odt) have their text extracted."
    }
    fn parameters(&self) -> Value {
        json!({ "type": "object", "properties": { "path": { "type": "string" } }, "required": ["path"] })
    }
    // read-only — safe to run concurrently.
    fn execution_mode(&self) -> ExecutionMode { ExecutionMode::Parallel }
    async fn execute(&self, _id: &str, args: Value, _cancel: &CancellationToken) -> anyhow::Result<ToolResult> {
        let path = args.get("path").and_then(Value::as_str).unwrap_or_default();
        let abs = resolve(&self.cwd, path)?;
        // Regular files only (no devices/FIFOs), and bounded in size.
        let meta = tokio::fs::metadata(&abs).await?;
        if !meta.is_file() {
            return Ok(ToolResult::text(format!("Error: {path} is not a regular file")));
        }
        if meta.len() > MAX_READ_BYTES {
            return Ok(ToolResult::text(format!(
                "Error: {path} is {} bytes; the read tool's limit is {MAX_READ_BYTES}. Use cli (head/tail/grep) for large files.",
                meta.len()
            )));
        }
        // Documents: extract readable text instead of failing on binary bytes.
        if crate::sdk::extract::is_document(&abs) {
            let bytes = tokio::fs::read(&abs).await?;
            return Ok(match crate::sdk::extract::extract_text(&abs, &bytes) {
                Some(text) => ToolResult::text(tail(&format!("[extracted document text]\n{text}"), 100_000)),
                None => ToolResult::text(format!(
                    "Error: could not extract readable text from {path} (possibly scanned, encrypted, or an unsupported encoding)."
                )),
            });
        }
        let content = tokio::fs::read_to_string(&abs).await?;
        Ok(ToolResult::text(tail(&content, 100_000)))
    }
}

// ── write ───────────────────────────────────────────────────────────────
pub struct WriteTool {
    pub cwd: PathBuf,
}

#[async_trait]
impl AgentTool for WriteTool {
    fn name(&self) -> &str { "write" }
    fn description(&self) -> &str { "Write content to a file (creating parent dirs)." }
    fn parameters(&self) -> Value {
        json!({ "type": "object", "properties": {
            "path": { "type": "string" }, "content": { "type": "string" }
        }, "required": ["path", "content"] })
    }
    fn execution_mode(&self) -> ExecutionMode { ExecutionMode::Sequential }
    async fn execute(&self, _id: &str, args: Value, _cancel: &CancellationToken) -> anyhow::Result<ToolResult> {
        let path = args.get("path").and_then(Value::as_str).unwrap_or_default();
        let content = args.get("content").and_then(Value::as_str).unwrap_or_default();
        let abs = resolve(&self.cwd, path)?;
        if let Some(parent) = abs.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&abs, content).await?;
        Ok(ToolResult::text(format!("Wrote {} bytes to {path}", content.len())))
    }
}

// ── memory ──────────────────────────────────────────────────────────────
pub struct Memory {
    pub dir: PathBuf,
}

#[async_trait]
impl AgentTool for Memory {
    fn name(&self) -> &str { "memory" }
    fn description(&self) -> &str { "Load or save git-committed agent memory." }
    fn parameters(&self) -> Value {
        json!({ "type": "object", "properties": {
            "action": { "type": "string", "enum": ["load", "save"] },
            "content": { "type": "string" },
            "message": { "type": "string" }
        }, "required": ["action"] })
    }
    fn execution_mode(&self) -> ExecutionMode { ExecutionMode::Sequential }
    async fn execute(&self, _id: &str, args: Value, _cancel: &CancellationToken) -> anyhow::Result<ToolResult> {
        let action = args.get("action").and_then(Value::as_str).unwrap_or("load");
        let path = self.dir.join("memory").join("MEMORY.md");
        match action {
            "load" => Ok(ToolResult::text(
                tokio::fs::read_to_string(&path).await.unwrap_or_else(|_| "No memories yet.".into()),
            )),
            "save" => {
                let content = args.get("content").and_then(Value::as_str).unwrap_or_default();
                if let Some(parent) = path.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                tokio::fs::write(&path, content).await?;
                let message = args.get("message").and_then(Value::as_str).unwrap_or("gitagent: update memory");
                let _ = Command::new("git").args(["add", "memory/MEMORY.md"]).current_dir(&self.dir).output().await;
                let _ = Command::new("git").args(["commit", "-m", message]).current_dir(&self.dir).output().await;
                Ok(ToolResult::text(format!("Saved {} bytes to memory.", content.len())))
            }
            other => Ok(ToolResult::text(format!("Unknown action '{other}'."))),
        }
    }
}

// ── edit ────────────────────────────────────────────────────────────────
pub struct EditTool {
    pub cwd: PathBuf,
}

#[async_trait]
impl AgentTool for EditTool {
    fn name(&self) -> &str { "edit" }
    fn description(&self) -> &str { "Replace an exact substring in a file (must be unique unless replace_all)." }
    fn parameters(&self) -> Value {
        json!({ "type": "object", "properties": {
            "path": { "type": "string" },
            "old_string": { "type": "string" },
            "new_string": { "type": "string" },
            "replace_all": { "type": "boolean" }
        }, "required": ["path", "old_string", "new_string"] })
    }
    fn execution_mode(&self) -> ExecutionMode { ExecutionMode::Sequential }
    async fn execute(&self, _id: &str, args: Value, _cancel: &CancellationToken) -> anyhow::Result<ToolResult> {
        let path = args.get("path").and_then(Value::as_str).unwrap_or_default();
        let old = args.get("old_string").and_then(Value::as_str).unwrap_or_default();
        let new = args.get("new_string").and_then(Value::as_str).unwrap_or_default();
        let all = args.get("replace_all").and_then(Value::as_bool).unwrap_or(false);
        let abs = resolve(&self.cwd, path)?;
        let original = tokio::fs::read_to_string(&abs).await?;
        if old.is_empty() || !original.contains(old) {
            return Ok(ToolResult::text(format!("Error: old_string not found in {path}")));
        }
        let count = original.matches(old).count();
        if !all && count > 1 {
            return Ok(ToolResult::text(format!(
                "Error: old_string matches {count} times in {path}; add context or set replace_all=true"
            )));
        }
        let updated = if all { original.replace(old, new) } else { original.replacen(old, new, 1) };
        tokio::fs::write(&abs, updated).await?;
        Ok(ToolResult::text(format!("Edited {path} ({} replacement(s))", if all { count } else { 1 })))
    }
}

// ── run_agent (subagent delegation) ─────────────────────────────────────
pub struct RunAgent {
    pub base_dir: PathBuf,
    pub sub_agents: Vec<String>,
    /// The parent's gates + mode, enforced on the sub-agent too.
    pub policy: DelegationPolicy,
}

#[async_trait]
impl AgentTool for RunAgent {
    fn name(&self) -> &str { "run_agent" }
    fn description(&self) -> &str {
        "Delegate a task to a named sub-agent (defined under agents/<name>/). Returns the sub-agent's final answer."
    }
    fn parameters(&self) -> Value {
        json!({ "type": "object", "properties": {
            "agent": { "type": "string", "enum": self.sub_agents },
            "prompt": { "type": "string", "description": "The task for the sub-agent" }
        }, "required": ["agent", "prompt"] })
    }
    // Parallel: each invocation gets an isolated workspace (below), so multiple
    // sub-agents fan out concurrently without racing on files.
    fn execution_mode(&self) -> ExecutionMode { ExecutionMode::Parallel }
    // …but delegating can run any tool the sub-agent has, so it counts as
    // mutating for plan mode.
    fn is_mutating(&self) -> bool { true }
    async fn execute(&self, id: &str, args: Value, cancel: &CancellationToken) -> anyhow::Result<ToolResult> {
        let name = args.get("agent").and_then(Value::as_str).unwrap_or_default().to_string();
        let prompt = args.get("prompt").and_then(Value::as_str).unwrap_or_default().to_string();
        if !self.sub_agents.iter().any(|s| s == &name) {
            return Ok(ToolResult::text(format!("Error: unknown sub-agent '{name}'")));
        }
        // Identity comes from the sub-agent's definition dir…
        let def_dir = self.base_dir.join("agents").join(&name);
        let loaded = {
            let d = def_dir.clone();
            tokio::task::spawn_blocking(move || load_agent(&d)).await??
        };
        let model = loaded.manifest.model.preferred.clone();

        // …but its file tools operate in a fresh, isolated workspace keyed by the
        // (unique) tool-call id — so parallel sub-agents never clobber each other.
        let safe_id: String = id.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
        let workspace = self.base_dir.join(".gitagent").join("sub").join(format!("{name}-{safe_id}"));
        tokio::fs::create_dir_all(&workspace).await.ok();

        let params = loaded
            .manifest
            .model
            .constraints
            .as_ref()
            .map(|c| GenParams { temperature: c.temperature, max_tokens: c.max_tokens, top_p: c.top_p })
            .unwrap_or_default();
        let mut tools = {
            let (ws, opts) = (workspace.clone(), ToolOptions {
                skip_agent_executables: false,
                delegation: self.policy.clone(),
            });
            tokio::task::spawn_blocking(move || builtin_tools_with(&ws, &opts)).await?
        };
        // The sub-agent's own manifest still applies: tool allow-list…
        if let Some(allow) = loaded.manifest.tools.as_ref() {
            tools.retain(|t| allow.iter().any(|a| a == t.name()));
        }
        // …and gates: the parent's (rules, hooks, plan mode) plus the
        // sub-agent's own permission rules under the parent's mode override.
        let mut gates: Vec<Arc<dyn ToolGate>> =
            self.policy.gates.read().unwrap_or_else(|e| e.into_inner()).clone();
        let mutating: HashSet<String> =
            tools.iter().filter(|t| t.is_mutating()).map(|t| t.name().to_string()).collect();
        let perm_cfg = loaded.manifest.permissions.clone().unwrap_or_else(PermissionConfig::default);
        let perm = PermissionGate::new(&perm_cfg, mutating, self.policy.permission_mode.as_deref()).with_root(&workspace);
        if !perm.is_noop() {
            gates.push(Arc::new(perm));
        }
        let agent = Agent::new(loaded.system_prompt, tools, &model)
            .with_fallbacks(&loaded.manifest.model.fallback)
            .with_params(params)
            .with_gates(gates)
            .with_max_turns(loaded.manifest.max_turns());

        // Drive the sub-agent to completion, collecting its text.
        let mut stream = agent.prompt(prompt);
        let mut text = String::new();
        loop {
            tokio::select! {
                _ = cancel.cancelled() => { agent.abort(); break; }
                ev = stream.next() => match ev {
                    Some(AgentEvent::MessageDelta { kind: DeltaKind::Text, text: t }) => text.push_str(&t),
                    Some(AgentEvent::AgentEnd) | None => break,
                    Some(_) => {}
                }
            }
        }
        Ok(ToolResult::text(if text.trim().is_empty() {
            format!("(sub-agent '{name}' produced no text)")
        } else {
            text
        }))
    }
}
