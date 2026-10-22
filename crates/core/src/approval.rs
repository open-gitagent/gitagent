//! Human approval of tool calls and final responses, and the kill switch
//! (see `gitagent_loader::compliance` for when approval is needed).
//!
//! Whoever runs the agent supplies an [`Approver`]: the terminal asks the
//! person at it, the IPC protocol asks the client. Without one, or when no
//! answer comes in time, the request is denied.

use std::path::PathBuf;
use std::sync::Arc;

use gitagent_loader::compliance::{Hitl, Trigger};
use gitagent_loader::AgentSpec;
use gitagent_provider::ToolCall;
use serde_json::{json, Map, Value};

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

/// Stops the agent once tripped: a model call in progress is cancelled,
/// and no further tool call starts. Clones share the switch.
#[derive(Clone)]
pub struct KillSwitch(Arc<tokio::sync::watch::Sender<Option<String>>>);

impl Default for KillSwitch {
    fn default() -> Self {
        KillSwitch(Arc::new(tokio::sync::watch::channel(None).0))
    }
}

impl KillSwitch {
    pub fn trip(&self) {
        self.stop("the kill switch was used");
    }

    /// Trips the switch with the reason the run reports (the first one
    /// given stays).
    pub fn stop(&self, reason: impl Into<String>) {
        let reason = reason.into();
        self.0.send_if_modified(|r| {
            if r.is_none() {
                *r = Some(reason);
                true
            } else {
                false
            }
        });
    }

    pub fn is_tripped(&self) -> bool {
        self.reason().is_some()
    }

    pub fn reason(&self) -> Option<String> {
        self.0.borrow().clone()
    }

    /// Waits until the switch is tripped and gives the reason.
    pub async fn wait(&self) -> String {
        let mut rx = self.0.subscribe();
        let reason = rx.wait_for(|r| r.is_some()).await.map(|r| r.clone());
        match reason {
            Ok(r) => r.unwrap_or_default(),
            // The sender never goes away while a run holds the switch.
            Err(_) => std::future::pending().await,
        }
    }
}

/// `YYYY-MM` of today.
fn this_month() -> String {
    gitagent_record::utc_now_rfc3339().chars().take(7).collect()
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
    /// Where this month's spending is kept (`.gitagent/state.json`).
    state_file: Option<PathBuf>,
}

/// A payment a tool call makes: the amount in cents and its category.
pub(crate) struct Payment {
    pub cents: u64,
    pub category: Option<String>,
}

/// The payment a call makes, read from `amount_cents` (or `amount`, in
/// whole units) and `category` in its arguments.
pub(crate) fn payment_of(call: &ToolCall) -> Option<Payment> {
    let args = &call.arguments;
    let cents = match (args.get("amount_cents"), args.get("amount")) {
        (Some(c), _) => c
            .as_u64()
            .or_else(|| c.as_str().and_then(|s| s.parse().ok()))?,
        (None, Some(a)) => {
            let units = a
                .as_f64()
                .or_else(|| a.as_str().and_then(|s| s.parse().ok()))?;
            if units < 0.0 {
                return None;
            }
            (units * 100.0).round() as u64
        }
        (None, None) => return None,
    };
    Some(Payment {
        cents,
        category: args
            .get("category")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
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
        let state_file = spec
            .agent
            .as_ref()
            .filter(|_| spec.supervision.finance.is_some())
            .map(|a| a.root.join(".gitagent/state.json"));
        Gate {
            spec,
            approver,
            mcp,
            kill,
            kill_file,
            error_seen: false,
            state_file,
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

    /// Notes a call's result: for the `error_detected` trigger, and a
    /// payment that went through counts toward the month's spending.
    pub fn observe(&mut self, call: &ToolCall, is_error: bool) {
        self.error_seen |= is_error;
        if is_error || self.spec.supervision.finance.is_none() {
            return;
        }
        if let Some(p) = payment_of(call) {
            let month = this_month();
            let mut state = self.state();
            let spent = state["spending"][&month].as_u64().unwrap_or(0);
            state["spending"][&month] = json!(spent + p.cents);
            if let Some(path) = &self.state_file {
                if let Some(dir) = path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                let _ = std::fs::write(path, state.to_string());
            }
        }
    }

    fn state(&self) -> Value {
        self.state_file
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_else(|| json!({}))
    }

    /// Why the call may not run at all, if so: a handoff whose roles the
    /// agent does not hold, or a payment outside the spending rules.
    pub fn block_reason(&self, call: &ToolCall) -> Option<String> {
        let sup = &self.spec.supervision;
        let agent = self
            .spec
            .agent
            .as_ref()
            .map(|a| a.manifest.name.as_str())
            .unwrap_or_default();
        if let Some(sod) = &sup.sod {
            if let Some(h) = sod.handoff_for(&call.name) {
                let held = sod.roles_of(agent);
                if sod.strict && !h.required_roles.iter().any(|r| held.contains(r)) {
                    return Some(format!(
                        "{} needs the roles {} (a handoff), and {agent} holds {}",
                        h.action,
                        h.required_roles.join(" → "),
                        if held.is_empty() {
                            "none".to_string()
                        } else {
                            held.join(", ")
                        }
                    ));
                }
            }
        }
        let (Some(f), Some(p)) = (&sup.finance, payment_of(call)) else {
            return None;
        };
        if let Some(c) = &p.category {
            if f.blocked_categories.iter().any(|b| b == c) {
                return Some(format!(
                    "the category {c} is blocked (financial governance)"
                ));
            }
            if !f.allowed_categories.is_empty() && !f.allowed_categories.iter().any(|a| a == c) {
                return Some(format!(
                    "the category {c} is not among the allowed ones (financial governance)"
                ));
            }
        } else if !f.allowed_categories.is_empty() {
            return Some(
                "the payment names no category, and only some are allowed (financial governance)"
                    .into(),
            );
        }
        if f.max_per_transaction_cents.is_some_and(|max| p.cents > max) {
            return Some(format!(
                "{} cents is over the limit of {} per transaction (financial governance)",
                p.cents,
                f.max_per_transaction_cents.unwrap_or_default()
            ));
        }
        if let Some(cap) = f.max_monthly_cents {
            let spent = self.state()["spending"][this_month()].as_u64().unwrap_or(0);
            if spent + p.cents > cap {
                return Some(format!(
                    "{} cents would take this month's spending to {} of the {cap} allowed (financial governance)",
                    p.cents,
                    spent + p.cents
                ));
            }
        }
        None
    }

    /// Whether a request that nobody answers in time counts as approved
    /// (a payment with `auto_deny_on_timeout: false`).
    pub fn approve_on_timeout(&self, call: &ToolCall) -> bool {
        match (&self.spec.supervision.finance, payment_of(call)) {
            (Some(f), Some(_)) => !f.auto_deny_on_timeout,
            _ => false,
        }
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
        if let Some(h) = sup.sod.as_ref().and_then(|s| s.handoff_for(name)) {
            if h.approval_required {
                return Some(format!(
                    "{} is a handoff through the roles {} (approval required)",
                    h.action,
                    h.required_roles.join(" → ")
                ));
            }
        }
        if let (Some(f), Some(p)) = (&sup.finance, payment_of(call)) {
            if f.require_above_cents.is_some_and(|t| p.cents > t) {
                return Some(format!(
                    "a payment of {} cents is over {} (financial governance)",
                    p.cents,
                    f.require_above_cents.unwrap_or_default()
                ));
            }
        }
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
        self.ask_with(request, false).await
    }

    /// Asks for approval; with `approve_on_timeout`, no answer in time
    /// counts as approval (a payment with `auto_deny_on_timeout: false`).
    pub async fn ask_with(
        &mut self,
        request: &ApprovalRequest,
        approve_on_timeout: bool,
    ) -> Approval {
        let Some(approver) = self.approver else {
            return Approval::denied("no one is available to approve it");
        };
        let timeout = self.spec.supervision.approval_timeout;
        let answer = tokio::time::timeout(timeout, approver.approve(request))
            .await
            .unwrap_or_else(|_| {
                let minutes = timeout.as_secs() / 60;
                if approve_on_timeout {
                    Approval {
                        approved: true,
                        reason: Some(format!("no answer within {minutes} minutes; approved as auto_deny_on_timeout is off")),
                    }
                } else {
                    Approval::denied(format!("no answer within {minutes} minutes"))
                }
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
