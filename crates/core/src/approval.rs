//! Human approval of tool calls and final responses, and the kill switch
//! (see `gitagent_loader::compliance` for when approval is needed).
//!
//! Whoever runs the agent supplies an [`Approver`]: the terminal asks the
//! person at it, the IPC protocol asks the client. Without one, or when no
//! answer comes in time, the request is denied.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use gitagent_loader::compliance::{Hitl, Trigger};
use gitagent_loader::AgentSpec;
use gitagent_provider::ToolCall;
use serde_json::{Map, Value};

/// A call (or a final response, as a `task_complete` call) waiting for a
/// person.
#[derive(Debug, Clone, PartialEq)]
pub struct ApprovalRequest {
    /// gitagent's name for the tool.
    pub tool_name: String,
    pub arguments: Map<String, Value>,
    /// Why approval is needed.
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Approval {
    pub approved: bool,
    pub reason: Option<String>,
}

impl Approval {
    pub fn denied(reason: impl Into<String>) -> Self {
        Approval {
            approved: false,
            reason: Some(reason.into()),
        }
    }
}

#[async_trait::async_trait]
pub trait Approver: Send + Sync {
    async fn approve(&self, request: &ApprovalRequest) -> Approval;
}

/// Stops the agent before its next model call or tool call once tripped.
/// Clones share the switch.
#[derive(Clone, Default)]
pub struct KillSwitch(Arc<Mutex<Option<String>>>);

impl KillSwitch {
    pub fn trip(&self) {
        self.stop("the kill switch was used");
    }

    /// Trips the switch with the reason the run reports (the first one
    /// given stays).
    pub fn stop(&self, reason: impl Into<String>) {
        let mut r = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if r.is_none() {
            *r = Some(reason.into());
        }
    }

    pub fn is_tripped(&self) -> bool {
        self.reason().is_some()
    }

    pub fn reason(&self) -> Option<String> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// Built-in tools that change nothing.
const READ_ONLY: &[&str] = &[
    gitagent_tools::READ_FILE,
    gitagent_tools::IMAGE_READ,
    gitagent_tools::TODO,
    gitagent_tools::SKILL,
    gitagent_tools::TASK_COMPLETE,
    // A sub-agent's own calls are asked about one by one.
    gitagent_tools::SUBAGENT,
];

/// Decides which calls wait for approval and asks for it.
pub(crate) struct Gate<'a> {
    spec: &'a AgentSpec,
    approver: Option<&'a Arc<dyn Approver>>,
    mcp: Option<&'a gitagent_mcp::Servers>,
    kill: Option<&'a KillSwitch>,
    kill_file: Option<PathBuf>,
    /// A call failed since the last approval (`error_detected`).
    error_seen: bool,
}

impl<'a> Gate<'a> {
    pub fn new(
        spec: &'a AgentSpec,
        approver: Option<&'a Arc<dyn Approver>>,
        mcp: Option<&'a gitagent_mcp::Servers>,
        kill: Option<&'a KillSwitch>,
    ) -> Self {
        let kill_file = spec
            .agent
            .as_ref()
            .filter(|_| spec.supervision.kill_switch)
            .map(|a| a.root.join(".gitagent/KILL"));
        Gate {
            spec,
            approver,
            mcp,
            kill,
            kill_file,
            error_seen: false,
        }
    }

    /// Why the agent must stop now, if it must.
    pub fn halted(&self) -> Option<String> {
        if let Some(reason) = self.kill.and_then(KillSwitch::reason) {
            return Some(reason);
        }
        self.kill_file
            .as_ref()
            .filter(|f| f.exists())
            .map(|f| format!("{} exists (remove it to run the agent again)", f.display()))
    }

    /// Notes a call's result for the `error_detected` trigger.
    pub fn observe(&mut self, is_error: bool) {
        self.error_seen |= is_error;
    }

    /// Whether the call can change anything.
    fn changes_something(&self, call: &ToolCall) -> bool {
        let name = call.name.as_str();
        if READ_ONLY.contains(&name) {
            return false;
        }
        if name == gitagent_tools::MEMORY {
            return call.arguments.get("action").and_then(Value::as_str) == Some("save");
        }
        if let Some(t) = self.spec.declared_tools.iter().find(|t| t.name == name) {
            return !t.read_only;
        }
        !self.mcp.is_some_and(|m| m.read_only(name))
    }

    /// Why the call needs approval, if it does; `None` also for advisory
    /// supervision, which only reports (see [`Gate::advisory`]).
    pub fn reason(&self, call: &ToolCall, output_tokens: u64) -> Option<String> {
        let name = call.name.as_str();
        if self
            .spec
            .declared_tools
            .iter()
            .any(|t| t.name == name && t.requires_confirmation)
        {
            return Some(format!("{name} asks for confirmation before each call"));
        }
        let sup = &self.spec.supervision;
        let changes = self.changes_something(call);
        match sup.hitl {
            Hitl::Always if changes => {
                Some("every action needs approval (human_in_the_loop: always)".into())
            }
            Hitl::Conditional => sup.triggers.iter().find_map(|t| match t {
                Trigger::ActionType(names) if names.iter().any(|n| n == name) => {
                    Some(format!("escalation trigger action_type: {}", names.last()?))
                }
                Trigger::ErrorDetected if changes && self.error_seen => {
                    Some("an earlier call failed (escalation trigger error_detected)".into())
                }
                Trigger::TokenCountAbove(n) if changes && output_tokens > *n => Some(format!(
                    "the session's output passed {n} tokens (escalation trigger token_count_above)"
                )),
                _ => None,
            }),
            _ => None,
        }
    }

    /// Whether a call would be reported under advisory supervision.
    pub fn advisory(&self, call: &ToolCall) -> bool {
        self.spec.supervision.hitl == Hitl::Advisory && self.changes_something(call)
    }

    /// Asks for approval; no approver or no answer in time means denied.
    pub async fn ask(&mut self, request: &ApprovalRequest) -> Approval {
        let Some(approver) = self.approver else {
            return Approval::denied("no one is available to approve it");
        };
        let timeout = self.spec.supervision.approval_timeout;
        let answer = tokio::time::timeout(timeout, approver.approve(request))
            .await
            .unwrap_or_else(|_| {
                Approval::denied(format!(
                    "no answer within {} minutes",
                    timeout.as_secs() / 60
                ))
            });
        if answer.approved {
            self.error_seen = false;
        }
        answer
    }
}

/// The tool result for a call that was not approved.
pub(crate) fn denial_message(request: &ApprovalRequest, answer: &Approval) -> String {
    let why = answer.reason.as_deref().unwrap_or("no reason given");
    format!(
        "Not run: this call needed approval ({}) and was denied: {why}. Do not repeat it unchanged; change the approach or explain what you need.",
        request.reason
    )
}
