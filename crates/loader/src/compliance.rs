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

use std::collections::BTreeMap;
use std::path::PathBuf;
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
    /// `segregation_of_duties`, when the section is present.
    pub sod: Option<Sod>,
    /// `financial_governance`, when `enabled: true`.
    pub finance: Option<Finance>,
}

/// Segregation of duties: which agent holds which roles, which roles
/// conflict, and which actions need a handoff.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Sod {
    pub roles: Vec<String>,
    pub conflicts: Vec<(String, String)>,
    /// Agent name to the roles it holds.
    pub assignments: BTreeMap<String, Vec<String>>,
    pub handoffs: Vec<Handoff>,
    /// `enforcement: strict` (the default): violations block.
    pub strict: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Handoff {
    /// The action, taken as a tool name (any naming), as `action_type` is.
    pub action: String,
    pub tools: Vec<String>,
    pub required_roles: Vec<String>,
    pub approval_required: bool,
}

impl Sod {
    pub fn roles_of(&self, agent: &str) -> &[String] {
        self.assignments.get(agent).map_or(&[], Vec::as_slice)
    }

    /// A conflicting pair of roles the agent holds, if any.
    pub fn conflict_of(&self, agent: &str) -> Option<(String, String)> {
        let held = self.roles_of(agent);
        self.conflicts
            .iter()
            .find(|(a, b)| held.contains(a) && held.contains(b))
            .cloned()
    }

    /// The handoff a tool call falls under, if any.
    pub fn handoff_for(&self, tool: &str) -> Option<&Handoff> {
        self.handoffs
            .iter()
            .find(|h| h.tools.iter().any(|t| t == tool))
    }
}

/// Spending limits for payment-like tool calls: a call whose arguments
/// carry `amount_cents` (or `amount`, in whole units) and, optionally, a
/// `category`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Finance {
    pub firewall: Option<String>,
    pub max_per_transaction_cents: Option<u64>,
    pub max_monthly_cents: Option<u64>,
    pub currency: Option<String>,
    pub allowed_categories: Vec<String>,
    pub blocked_categories: Vec<String>,
    pub require_above_cents: Option<u64>,
    /// A request nobody answers in time counts as denied (true) or
    /// approved (false).
    pub auto_deny_on_timeout: bool,
}

/// Where and what the audit log records (`recordkeeping`).
#[derive(Debug, Clone, PartialEq)]
pub struct AuditCfg {
    pub dir: PathBuf,
    /// One file for everything (gitagent 1.x's `.gitagent/audit.jsonl`)
    /// instead of a file per day in `dir`.
    pub file: Option<PathBuf>,
    /// `log_contents`; empty means everything.
    pub contents: Vec<String>,
    pub immutable: bool,
}

/// Whether a `compliance` section is written the gitagent 1.x way.
pub fn is_v1_shape(compliance: &Value) -> bool {
    compliance.get("risk_level").is_some()
        || compliance["human_in_the_loop"].is_boolean()
        || compliance["recordkeeping"].get("retention_days").is_some()
}

impl AuditCfg {
    pub fn logs(&self, category: &str) -> bool {
        self.contents.is_empty() || self.contents.iter().any(|c| c == category)
    }
}

impl Default for Supervision {
    fn default() -> Self {
        Supervision {
            hitl: Hitl::None,
            triggers: Vec::new(),
            kill_switch: false,
            review_responses: false,
            approval_timeout: DEFAULT_APPROVAL_TIMEOUT,
            sod: None,
            finance: None,
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
        // 1.x's `human_in_the_loop: true` only asked for review in the
        // prompt; nothing waited for a person, so it reports rather than
        // gates.
        None if compliance["human_in_the_loop"] == Value::Bool(true) => Hitl::Advisory,
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
        sod: read_sod(&compliance["segregation_of_duties"]),
        finance: read_finance(&compliance["financial_governance"]),
    };
    (supervision, prompt_lines(compliance))
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

fn read_sod(v: &Value) -> Option<Sod> {
    let v = v.as_object()?;
    let v = Value::Object(v.clone());
    let roles: Vec<String> = v["roles"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| r["id"].as_str().or(r.as_str()))
        .map(str::to_string)
        .collect();
    let conflicts = v["conflicts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| Some((p[0].as_str()?.to_string(), p[1].as_str()?.to_string())))
        .collect();
    let assignments = v["assignments"]
        .as_object()
        .map(|m| m.iter().map(|(k, r)| (k.clone(), strings(r))).collect())
        .unwrap_or_default();
    let handoffs = v["handoffs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|h| {
            let action = h["action"].as_str()?.trim().to_string();
            let mut tools = crate::sdk::normalize_tool_list(std::slice::from_ref(&action));
            if !tools.contains(&action) {
                tools.push(action.clone());
            }
            Some(Handoff {
                action,
                tools,
                required_roles: strings(&h["required_roles"]),
                approval_required: h["approval_required"] != Value::Bool(false),
            })
        })
        .collect();
    Some(Sod {
        roles,
        conflicts,
        assignments,
        handoffs,
        strict: v["enforcement"].as_str() != Some("advisory"),
    })
}

fn read_finance(v: &Value) -> Option<Finance> {
    if v["enabled"] != Value::Bool(true) {
        return None;
    }
    let spending = &v["spending"];
    let approval = &v["approval"];
    Some(Finance {
        firewall: v["firewall"].as_str().map(str::to_string),
        max_per_transaction_cents: spending["max_per_transaction_cents"].as_u64(),
        max_monthly_cents: spending["max_monthly_cents"].as_u64(),
        currency: spending["currency"].as_str().map(str::to_string),
        allowed_categories: strings(&spending["allowed_categories"]),
        blocked_categories: strings(&spending["blocked_categories"]),
        require_above_cents: approval["require_above_cents"].as_u64(),
        auto_deny_on_timeout: approval["auto_deny_on_timeout"] != Value::Bool(false),
    })
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

/// What 1.x put in the prompt from `compliance/regulatory-map.yaml` (the
/// frameworks it names) and `compliance/validation-schedule.yaml` (its
/// checks). Unreadable files are left out, as in 1.x.
pub fn files_section(regulatory_map: Option<&str>, validation_schedule: Option<&str>) -> String {
    let yaml = |text: Option<&str>| text.and_then(|t| serde_saphyr::from_str::<Value>(t).ok());
    let mut parts = Vec::new();
    if let Some(map) = yaml(regulatory_map) {
        let names: Vec<String> = match &map["frameworks"] {
            Value::Object(o) => o.keys().cloned().collect(),
            Value::Array(a) => a
                .iter()
                .filter_map(|f| f.as_str().or(f["name"].as_str()))
                .map(str::to_string)
                .collect(),
            _ => Vec::new(),
        };
        if !names.is_empty() {
            parts.push(format!("Regulatory frameworks: {}", names.join(", ")));
        }
    }
    if let Some(schedule) = yaml(validation_schedule) {
        let checks: Vec<String> = schedule["checks"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| {
                let mut line = format!("- {}", c["name"].as_str()?);
                if let Some(f) = c["frequency"].as_str() {
                    line.push_str(&format!(" ({f})"));
                }
                if let Some(d) = c["description"].as_str().filter(|d| !d.is_empty()) {
                    line.push_str(&format!(": {d}"));
                }
                Some(line)
            })
            .collect();
        if !checks.is_empty() {
            parts.push(format!("Validation schedule:\n{}", checks.join("\n")));
        }
    }
    if parts.is_empty() {
        return String::new();
    }
    format!("# Compliance\n\n{}\n", parts.join("\n\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_1x_shape_is_advisory_not_gating() {
        let c = json!({
            "risk_level": "medium",
            "human_in_the_loop": true,
            "regulatory_frameworks": ["SOC2"],
            "recordkeeping": {"audit_logging": true, "retention_days": 90}
        });
        assert!(is_v1_shape(&c));
        let (s, _) = read(&c);
        assert_eq!(s.hitl, Hitl::Advisory);
        assert!(!s.gates());
        let (s, _) = read(&json!({"human_in_the_loop": false}));
        assert_eq!(s.hitl, Hitl::None);
        assert!(!is_v1_shape(&json!({"risk_tier": "high"})));
    }

    #[test]
    fn the_1x_compliance_files_are_summed_up_for_the_prompt() {
        let map = "frameworks:\n  SOC2: {controls: [CC6.1]}\n  GDPR: {articles: [32]}\n";
        let schedule = "checks:\n  - {name: Access review, frequency: quarterly, description: Review who has access}\n  - {name: Model drift}\n";
        // Framework names in sorted order.
        assert_eq!(
            files_section(Some(map), Some(schedule)),
            "# Compliance\n\nRegulatory frameworks: GDPR, SOC2\n\nValidation schedule:\n- Access review (quarterly): Review who has access\n- Model drift\n"
        );
        assert_eq!(
            files_section(Some("frameworks: [HIPAA, {name: SOX}]\n"), None),
            "# Compliance\n\nRegulatory frameworks: HIPAA, SOX\n"
        );
        // Missing, empty or unreadable files add nothing.
        assert_eq!(files_section(None, Some("checks: [\n")), "");
        assert_eq!(
            files_section(Some("frameworks: {}\n"), Some("checks: []\n")),
            ""
        );
    }

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
        let f = s.finance.as_ref().unwrap();
        assert_eq!(f.require_above_cents, Some(0));
        assert!(f.auto_deny_on_timeout && f.max_per_transaction_cents.is_none());
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
        let sod = s.sod.as_ref().unwrap();
        assert_eq!(sod.roles_of("checker"), ["review", "approve"]);
        assert_eq!(
            sod.conflicts,
            vec![("create".to_string(), "approve".to_string())]
        );
        assert!(sod.conflict_of("maker").is_none() && sod.strict);
        let h = sod.handoff_for("payment").unwrap();
        assert!(h.approval_required && h.required_roles == ["create", "approve"]);
        assert!(s.finance.is_none());
        assert!(prompt.contains("  - Agent \"checker\" has role(s): review, approve\n"));
        assert!(prompt.contains("  - create and approve\n"));
        assert!(prompt.contains(
            "  - payment: must pass through roles create → approve (approval required)\n"
        ));
        assert!(prompt.ends_with("- SOD enforcement is STRICT — violations will block execution\n"));
        assert_eq!(read(&json!({})).1, "");
    }
}
