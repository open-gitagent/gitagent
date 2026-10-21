//! The parts of `agent.yaml`'s `compliance` section that change how the
//! agent runs: who must approve what (`supervision`), the kill switch, and
//! the constraint lines added to the system prompt.
//!
//! `human_in_the_loop`:
//! - `always`: every tool call that can change something waits for a
//!   person's approval;
//! - `conditional`: only when an escalation trigger fires (`action_type`
//!   names a tool; `error_detected` after a failed call; `token_count_above`
//!   once the session's output passes the count; the other triggers cannot
//!   be evaluated and only appear in the prompt);
//! - `advisory`: nothing waits; such calls are reported as events;
//! - `none` or absent: nothing.
//!
//! `communications.pre_review_required` makes the final response wait for
//! approval as well. A request nobody answers is denied.

use std::time::Duration;

use serde_json::Value;

/// How long an approval request waits before it counts as denied.
pub const DEFAULT_APPROVAL_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Hitl {
    #[default]
    None,
    Advisory,
    Conditional,
    Always,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Trigger {
    /// Calls of these tools (gitagent names, plus the names as written).
    ActionType(Vec<String>),
    ErrorDetected,
    TokenCountAbove(u64),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Supervision {
    pub hitl: Hitl,
    pub triggers: Vec<Trigger>,
    pub kill_switch: bool,
    /// The final response needs approval too.
    pub review_responses: bool,
    pub approval_timeout: Duration,
}

impl Default for Supervision {
    fn default() -> Self {
        Supervision {
            hitl: Hitl::None,
            triggers: Vec::new(),
            kill_switch: false,
            review_responses: false,
            approval_timeout: DEFAULT_APPROVAL_TIMEOUT,
        }
    }
}

impl Supervision {
    /// Whether anything ever waits for a person.
    pub fn gates(&self) -> bool {
        matches!(self.hitl, Hitl::Always | Hitl::Conditional) || self.review_responses
    }
}

/// The supervision settings and the prompt's constraint lines.
pub fn read(compliance: &Value) -> (Supervision, String) {
    let sup = &compliance["supervision"];
    let hitl = match sup["human_in_the_loop"].as_str() {
        Some("always") => Hitl::Always,
        Some("conditional") => Hitl::Conditional,
        Some("advisory") => Hitl::Advisory,
        _ => Hitl::None,
    };
    let mut triggers = Vec::new();
    for t in sup["escalation_triggers"].as_array().into_iter().flatten() {
        if let Some(action) = t["action_type"].as_str() {
            let mut names = crate::sdk::normalize_tool_list(&[action.to_string()]);
            if !names.iter().any(|n| n == action) {
                names.push(action.to_string());
            }
            triggers.push(Trigger::ActionType(names));
        } else if t["error_detected"] == Value::Bool(true) {
            triggers.push(Trigger::ErrorDetected);
        } else if let Some(n) = t["token_count_above"].as_u64() {
            triggers.push(Trigger::TokenCountAbove(n));
        }
    }
    let approval_timeout = compliance["financial_governance"]["approval"]["timeout_minutes"]
        .as_u64()
        .filter(|m| *m >= 1)
        .map_or(DEFAULT_APPROVAL_TIMEOUT, |m| Duration::from_secs(m * 60));
    let supervision = Supervision {
        hitl,
        triggers,
        kill_switch: sup["kill_switch"] == Value::Bool(true),
        review_responses: compliance["communications"]["pre_review_required"] == Value::Bool(true),
        approval_timeout,
    };
    (supervision, prompt_lines(compliance))
}

/// The constraint lines of the OpenGAP prompt, in its order and wording.
fn prompt_lines(c: &Value) -> String {
    let mut lines: Vec<String> = Vec::new();
    let sup = &c["supervision"];
    if sup["human_in_the_loop"] == "always" {
        lines.push("- All decisions require human approval before execution".into());
    }
    if let Some(triggers) = sup["escalation_triggers"]
        .as_array()
        .filter(|t| !t.is_empty())
    {
        lines.push("- Escalate to human supervisor when:".into());
        for t in triggers {
            for (k, v) in t.as_object().into_iter().flatten() {
                let v = v.as_str().map_or_else(|| v.to_string(), str::to_string);
                lines.push(format!("  - {k}: {v}"));
            }
        }
    }
    let comms = &c["communications"];
    if comms["fair_balanced"] == Value::Bool(true) {
        lines.push("- All communications must be fair and balanced (FINRA 2210)".into());
    }
    if comms["no_misleading"] == Value::Bool(true) {
        lines.push("- Never make misleading, exaggerated, or promissory statements".into());
    }
    match c["data_governance"]["pii_handling"].as_str() {
        Some("redact") => {
            lines.push("- Redact all PII from outputs and intermediate reasoning".into())
        }
        Some("prohibit") => {
            lines.push("- Do not process any personally identifiable information".into())
        }
        _ => {}
    }
    let sod = &c["segregation_of_duties"];
    if sod.is_object() {
        lines.push("- Segregation of duties is enforced:".into());
        for (agent, roles) in sod["assignments"].as_object().into_iter().flatten() {
            let roles: Vec<&str> = roles
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            lines.push(format!(
                "  - Agent \"{agent}\" has role(s): {}",
                roles.join(", ")
            ));
        }
        if let Some(conflicts) = sod["conflicts"].as_array().filter(|c| !c.is_empty()) {
            lines.push("- Duty separation rules (no single agent may hold both):".into());
            for pair in conflicts {
                let p: Vec<&str> = pair
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect();
                if p.len() == 2 {
                    lines.push(format!("  - {} and {}", p[0], p[1]));
                }
            }
        }
        if let Some(handoffs) = sod["handoffs"].as_array().filter(|h| !h.is_empty()) {
            lines.push("- The following actions require multi-agent handoff:".into());
            for h in handoffs {
                let roles: Vec<&str> = h["required_roles"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect();
                let approval = if h["approval_required"] == Value::Bool(false) {
                    ""
                } else {
                    " (approval required)"
                };
                lines.push(format!(
                    "  - {}: must pass through roles {}{approval}",
                    h["action"].as_str().unwrap_or_default(),
                    roles.join(" → ")
                ));
            }
        }
        if sod["isolation"]["state"] == "full" {
            lines.push("- Agent state/memory is fully isolated per role — do not access another agent's state".into());
        }
        if sod["isolation"]["credentials"] == "separate" {
            lines.push("- Credentials are segregated per role — use only credentials assigned to your role".into());
        }
        if sod["enforcement"].as_str().unwrap_or("strict") == "strict" {
            lines.push("- SOD enforcement is STRICT — violations will block execution".into());
        }
    }
    if lines.is_empty() {
        return String::new();
    }
    format!("# Compliance constraints\n\n{}\n", lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn supervision_and_prompt_lines() {
        let c = json!({
            "risk_tier": "high",
            "supervision": {
                "human_in_the_loop": "conditional",
                "escalation_triggers": [
                    {"action_type": "cli"},
                    {"error_detected": true},
                    {"token_count_above": 5000},
                    {"confidence_below": 0.7}
                ],
                "kill_switch": true
            },
            "communications": {"pre_review_required": true, "no_misleading": true},
            "data_governance": {"pii_handling": "redact"},
            "financial_governance": {"enabled": true, "approval": {"require_above_cents": 0, "timeout_minutes": 2, "auto_deny_on_timeout": true}}
        });
        let (s, prompt) = read(&c);
        assert_eq!(s.hitl, Hitl::Conditional);
        assert_eq!(
            s.triggers,
            vec![
                Trigger::ActionType(vec!["exec".into(), "write_stdin".into(), "cli".into()]),
                Trigger::ErrorDetected,
                Trigger::TokenCountAbove(5000)
            ]
        );
        assert!(s.kill_switch && s.review_responses && s.gates());
        assert_eq!(s.approval_timeout, Duration::from_secs(120));
        assert_eq!(
            prompt,
            "# Compliance constraints\n\n- Escalate to human supervisor when:\n  - action_type: cli\n  - error_detected: true\n  - token_count_above: 5000\n  - confidence_below: 0.7\n- Never make misleading, exaggerated, or promissory statements\n- Redact all PII from outputs and intermediate reasoning\n"
        );

        let (s, prompt) = read(&json!({
            "supervision": {"human_in_the_loop": "advisory"},
            "segregation_of_duties": {
                "assignments": {"maker": ["create"], "checker": ["review", "approve"]},
                "conflicts": [["create", "approve"]],
                "handoffs": [{"action": "payment", "required_roles": ["create", "approve"]}],
                "isolation": {"state": "full"}
            }
        }));
        assert_eq!(s.hitl, Hitl::Advisory);
        assert!(!s.gates() && !s.kill_switch);
        assert!(prompt.contains("  - Agent \"checker\" has role(s): review, approve\n"));
        assert!(prompt.contains("  - create and approve\n"));
        assert!(prompt.contains(
            "  - payment: must pass through roles create → approve (approval required)\n"
        ));
        assert!(prompt.ends_with("- SOD enforcement is STRICT — violations will block execution\n"));
        assert_eq!(read(&json!({})).1, "");
    }
}
