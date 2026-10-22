//! Runs a workflow (see `gitagent_loader::workflows`): the steps in their
//! dependency order, each skipped when its conditions do not hold, asked
//! about when it needs approval, retried as it says, and otherwise run as
//! what it names: a tool call made directly, a sub-agent, or a turn of the
//! agent with the step's action (and skill) as its instruction. What a step
//! produces is kept under `steps.<id>.outputs` for the steps after it.

use std::collections::BTreeMap;
use std::time::Duration;

use gitagent_loader::workflows::{holds, substitute, Backoff, OnFailure, Step, Workflow};
use gitagent_loader::AgentSpec;
use gitagent_provider::{Content, Provider, ToolCall};
use serde_json::{json, Map, Value};

use crate::{
    error_output, run, run_subagent, Approval, ApprovalRequest, ChildRun, Outcome, RunOptions,
    ToolOutput,
};

/// Model turns a step may take.
const STEP_MAX_TURNS: u32 = 30;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepStatus {
    Done,
    Skipped,
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct StepResult {
    pub id: String,
    pub status: StepStatus,
    pub attempts: u32,
    pub outputs: Map<String, Value>,
}

#[derive(Debug, Clone)]
pub struct WorkflowReport {
    pub name: String,
    pub steps: Vec<StepResult>,
    /// The workflow's named outputs, taken from the steps that produced
    /// them.
    pub outputs: Map<String, Value>,
    /// Why the run stopped early, if it did.
    pub stopped: Option<String>,
}

impl WorkflowReport {
    pub fn succeeded(&self) -> bool {
        self.stopped.is_none()
            && self
                .steps
                .iter()
                .all(|s| !matches!(s.status, StepStatus::Failed(_)))
    }

    /// The report as text for the model or the terminal.
    pub fn text(&self) -> String {
        let mut out = format!("Workflow `{}`:", self.name);
        for s in &self.steps {
            let status = match &s.status {
                StepStatus::Done => "done".to_string(),
                StepStatus::Skipped => "skipped (its conditions did not hold)".to_string(),
                StepStatus::Failed(why) => format!("failed: {why}"),
            };
            let tries = if s.attempts > 1 {
                format!(" (after {} attempts)", s.attempts)
            } else {
                String::new()
            };
            out.push_str(&format!("\n- {}: {status}{tries}", s.id));
            if !s.outputs.is_empty() {
                out.push_str(&format!(
                    "\n  outputs: {}",
                    Value::Object(s.outputs.clone())
                ));
            }
        }
        if let Some(why) = &self.stopped {
            out.push_str(&format!("\nStopped: {why}"));
        }
        if !self.outputs.is_empty() {
            out.push_str(&format!(
                "\nOutputs: {}",
                Value::Object(self.outputs.clone())
            ));
        }
        out
    }
}

/// Inputs from the call's `inputs`, with defaults filled in; missing
/// required inputs are an error.
fn inputs_for(wf: &Workflow, given: &Map<String, Value>) -> Result<Map<String, Value>, String> {
    let mut inputs = given.clone();
    let mut missing = Vec::new();
    for i in &wf.inputs {
        if inputs.get(&i.name).is_none_or(Value::is_null) {
            match &i.default {
                Some(d) => {
                    inputs.insert(i.name.clone(), d.clone());
                }
                None if i.required => missing.push(i.name.clone()),
                None => {}
            }
        }
    }
    if missing.is_empty() {
        Ok(inputs)
    } else {
        Err(format!("missing inputs: {}", missing.join(", ")))
    }
}

/// Runs the workflow named `name` with `inputs`.
pub(crate) async fn run_workflow(
    spec: &AgentSpec,
    provider: &dyn Provider,
    name: &str,
    given: &Map<String, Value>,
    child: ChildRun,
) -> Result<WorkflowReport, String> {
    let Some(wf) = spec.workflows.iter().find(|w| w.name == name) else {
        let names: Vec<&str> = spec.workflows.iter().map(|w| w.name.as_str()).collect();
        return Err(format!(
            "no workflow named {name:?}; workflows: {}",
            names.join(", ")
        ));
    };
    if wf.is_narrative() {
        return Err(format!(
            "{name} is a written procedure ({}) with no steps to run; read it instead",
            wf.file
        ));
    }
    let inputs = inputs_for(wf, given)?;
    let order = wf.ordered_steps().map_err(|cycle| {
        format!(
            "the steps {} depend on each other in a cycle",
            cycle.join(", ")
        )
    })?;
    let mut ctx = json!({"inputs": inputs, "steps": {}, "trigger": {}});
    let mut report = WorkflowReport {
        name: name.to_string(),
        steps: Vec::new(),
        outputs: Map::new(),
        stopped: None,
    };
    let started = std::time::Instant::now();
    let remaining = move || child.remaining.map(|r| r.saturating_sub(started.elapsed()));
    for (n, step) in order.iter().enumerate() {
        let mut result = StepResult {
            id: step.id.clone(),
            status: StepStatus::Done,
            attempts: 0,
            outputs: Map::new(),
        };
        if !step.conditions.iter().all(|c| holds(c, &ctx)) {
            result.status = StepStatus::Skipped;
            ctx["steps"][&step.id] = json!({"status": "skipped", "outputs": {}});
            report.steps.push(result);
            continue;
        }
        let step_inputs = match substitute(&Value::Object(step.inputs.clone()), &ctx) {
            Value::Object(m) => m,
            _ => Map::new(),
        };
        if step.requires_approval {
            let request = ApprovalRequest {
                tool_name: gitagent_tools::WORKFLOW.into(),
                arguments: json!({"workflow": name, "step": step.id, "action": step.action, "inputs": step_inputs})
                    .as_object()
                    .cloned()
                    .unwrap_or_default(),
                reason: format!("the workflow step {} requires approval", step.id),
            };
            let answer = ask(&child, spec, &request).await;
            if !answer.approved {
                let why = format!(
                    "not approved: {}",
                    answer.reason.as_deref().unwrap_or("no reason given")
                );
                if !handle_failure(
                    wf,
                    step,
                    &why,
                    &child,
                    spec,
                    &mut report,
                    &mut ctx,
                    &mut result,
                )
                .await
                {
                    break;
                }
                continue;
            }
        }
        let max_attempts = if wf.on_step_failure == OnFailure::Retry && step.max_attempts == 1 {
            3
        } else {
            step.max_attempts
        };
        let mut last_error = String::new();
        let mut done = false;
        for attempt in 1..=max_attempts {
            result.attempts = attempt;
            if attempt > 1 {
                let wait = match step.backoff {
                    Backoff::Linear => Duration::from_secs(u64::from(attempt - 1)),
                    Backoff::Exponential => Duration::from_secs(1 << (attempt - 2).min(6)),
                };
                tokio::time::sleep(wait).await;
            }
            let out_dir = child
                .out_dir
                .join(format!("steps/{}-{}", n + 1, step.id))
                .join(format!("attempt-{attempt}"));
            match run_step(
                spec,
                provider,
                wf,
                step,
                &step_inputs,
                &child,
                out_dir,
                remaining(),
            )
            .await
            {
                Ok(text) => {
                    result.outputs = outputs_of(step, &text);
                    done = true;
                    break;
                }
                Err(e) => last_error = e,
            }
        }
        if done {
            ctx["steps"][&step.id] = json!({"status": "done", "outputs": result.outputs});
            for name in &step.outputs {
                if let Some(v) = result.outputs.get(name) {
                    if wf.outputs.contains(name) {
                        report.outputs.insert(name.clone(), v.clone());
                    }
                }
            }
            report.steps.push(result);
        } else if !handle_failure(
            wf,
            step,
            &last_error,
            &child,
            spec,
            &mut report,
            &mut ctx,
            &mut result,
        )
        .await
        {
            break;
        }
    }
    Ok(report)
}

/// Records a failed step and decides whether the workflow goes on, by
/// `error_handling.on_step_failure`.
#[allow(clippy::too_many_arguments)]
async fn handle_failure(
    wf: &Workflow,
    step: &Step,
    why: &str,
    child: &ChildRun,
    spec: &AgentSpec,
    report: &mut WorkflowReport,
    ctx: &mut Value,
    result: &mut StepResult,
) -> bool {
    result.status = StepStatus::Failed(why.to_string());
    ctx["steps"][&step.id] = json!({"status": "failed", "outputs": {}, "error": why});
    report.steps.push(result.clone());
    match wf.on_step_failure {
        OnFailure::Skip => true,
        OnFailure::Abort | OnFailure::Retry => {
            report.stopped = Some(format!("step {} failed: {why}", step.id));
            false
        }
        OnFailure::Escalate => {
            let target = wf.escalation_target.as_deref().unwrap_or("the supervisor");
            let request = ApprovalRequest {
                tool_name: gitagent_tools::WORKFLOW.into(),
                arguments: json!({"workflow": wf.name, "step": step.id, "error": why, "escalation_target": target})
                    .as_object()
                    .cloned()
                    .unwrap_or_default(),
                reason: format!(
                    "step {} of workflow {} failed ({why}); escalated to {target}: approve to go on without it",
                    step.id, wf.name
                ),
            };
            let answer = ask(child, spec, &request).await;
            if answer.approved {
                true
            } else {
                report.stopped = Some(format!(
                    "step {} failed and {target} did not approve going on: {}",
                    step.id,
                    answer.reason.as_deref().unwrap_or("no reason given")
                ));
                false
            }
        }
    }
}

async fn ask(child: &ChildRun, spec: &AgentSpec, request: &ApprovalRequest) -> Approval {
    let Some(approver) = &child.approver else {
        return Approval::denied("no one is available to approve it");
    };
    let timeout = spec.supervision.approval_timeout;
    tokio::time::timeout(timeout, approver.approve(request))
        .await
        .unwrap_or_else(|_| {
            Approval::denied(format!(
                "no answer within {} minutes",
                timeout.as_secs() / 60
            ))
        })
}

/// Runs one step and gives what it produced as text.
#[allow(clippy::too_many_arguments)]
async fn run_step(
    spec: &AgentSpec,
    provider: &dyn Provider,
    wf: &Workflow,
    step: &Step,
    inputs: &Map<String, Value>,
    child: &ChildRun,
    out_dir: std::path::PathBuf,
    remaining: Option<Duration>,
) -> Result<String, String> {
    if let Some(tool) = &step.tool {
        let call = ToolCall {
            id: format!("wf-{}", step.id),
            name: tool.clone(),
            arguments: inputs.clone(),
        };
        if let Some(t) = spec.declared_tools.iter().find(|t| &t.name == tool) {
            let out = gitagent_tools::run_declared(&call, t, remaining, &spec.env_config.env_vars)
                .await
                .map_err(|e| e.to_string())?;
            return if out.is_error {
                Err(out.content)
            } else {
                Ok(out.content)
            };
        }
        if let Some(servers) = child.mcp.as_ref().filter(|m| m.has(tool)) {
            let out = servers
                .call(tool, inputs, remaining.unwrap_or(Duration::from_secs(600)))
                .await?;
            return if out.is_error {
                Err(out.text)
            } else {
                Ok(out.text)
            };
        }
        // A built-in tool: the agent makes the call.
        return model_step(spec, provider, wf, step, inputs, child, out_dir, remaining).await;
    }
    if let Some(agent) = &step.agent {
        let prompt = step_prompt(spec, wf, step, inputs);
        let call = ToolCall {
            id: format!("wf-{}", step.id),
            name: gitagent_tools::SUBAGENT.into(),
            arguments: json!({"agent": agent, "description": step.id, "prompt": prompt})
                .as_object()
                .cloned()
                .unwrap_or_default(),
        };
        let run = run_subagent(
            spec,
            provider,
            &call,
            ChildRun {
                cwd: child.cwd.clone(),
                out_dir,
                remaining,
                sandbox: child.sandbox.clone(),
                hooks: child.hooks.clone(),
                mcp: child.mcp.clone(),
                approver: child.approver.clone(),
                kill: child.kill.clone(),
            },
        )
        .await;
        return if run.out.is_error && !run.partial {
            Err(run.out.content)
        } else {
            Ok(run.answer)
        };
    }
    model_step(spec, provider, wf, step, inputs, child, out_dir, remaining).await
}

/// The instruction for a step done by the agent or a sub-agent.
fn step_prompt(
    spec: &AgentSpec,
    wf: &Workflow,
    step: &Step,
    inputs: &Map<String, Value>,
) -> String {
    let mut text = format!(
        "# Workflow {}, step {}\n\n{}\n",
        wf.name,
        step.id,
        if step.action.is_empty() {
            "Do this step."
        } else {
            step.action.as_str()
        }
    );
    if let Some(name) = &step.skill {
        match spec.skills.iter().find(|s| &s.name == name) {
            Some(skill) => {
                let body = std::fs::read_to_string(&skill.path).unwrap_or_default();
                let body = gitagent_loader::split_frontmatter(&body)
                    .1
                    .trim()
                    .to_string();
                text.push_str(&format!("\nFollow the skill `{name}`:\n\n{body}\n"));
            }
            None => text.push_str(&format!(
                "\n(The skill `{name}` is not available; do your best without it.)\n"
            )),
        }
    }
    if let Some(tool) = &step.tool {
        text.push_str(&format!("\nUse the `{tool}` tool for this step.\n"));
    }
    if !inputs.is_empty() {
        text.push_str(&format!(
            "\n# Inputs\n\n```json\n{}\n```\n",
            serde_json::to_string_pretty(inputs).unwrap_or_default()
        ));
    }
    if !step.outputs.is_empty() {
        text.push_str(&format!(
            "\nWhen done, end your reply with a JSON object giving these outputs: {}.\n",
            step.outputs.join(", ")
        ));
    }
    text
}

/// A step done by a turn of the agent itself.
#[allow(clippy::too_many_arguments)]
async fn model_step(
    spec: &AgentSpec,
    provider: &dyn Provider,
    wf: &Workflow,
    step: &Step,
    inputs: &Map<String, Value>,
    child: &ChildRun,
    out_dir: std::path::PathBuf,
    remaining: Option<Duration>,
) -> Result<String, String> {
    let mut agent = spec.clone();
    agent.conversation = true;
    agent.profile.snapshot = false;
    agent.config.max_turns = STEP_MAX_TURNS;
    agent.agent_hooks.retain(|h| h.event.is_tool_event());
    // No workflows inside a workflow step.
    agent.workflows.clear();
    let opts = RunOptions {
        instruction: step_prompt(spec, wf, step, inputs),
        work_dir: child.cwd.clone(),
        out_dir,
        timeout: remaining,
        session_id: None,
        events: gitagent_record::Events::disabled(),
        resume: None,
        sandbox: child.sandbox.clone(),
        tool_allowlist: None,
        tool_denylist: Vec::new(),
        hook_callbacks: child.hooks.clone(),
        mcp: child.mcp.clone(),
        approver: child.approver.clone(),
        kill: child.kill.clone(),
        steer: None,
        external_tools: None,
    };
    let nested: std::pin::Pin<
        Box<dyn std::future::Future<Output = anyhow::Result<crate::Finished>> + Send + '_>,
    > = Box::pin(run(&agent, provider, opts));
    let finished = nested.await.map_err(|e| format!("{e:#}"))?;
    match finished.report.outcome {
        Outcome::Completed => Ok(finished.report.summary.unwrap_or_else(|| {
            finished
                .messages
                .iter()
                .rev()
                .filter(|m| m.role == gitagent_provider::Role::Assistant)
                .flat_map(|m| &m.content)
                .find_map(|c| match c {
                    Content::Text { text } => Some(text.clone()),
                    _ => None,
                })
                .unwrap_or_default()
        })),
        other => Err(format!("the step ended with {other}")),
    }
}

/// The step's named outputs from its reply: a JSON object in the text
/// (the last one), else the whole text under the first output name.
fn outputs_of(step: &Step, text: &str) -> Map<String, Value> {
    if step.outputs.is_empty() {
        return Map::new();
    }
    if let Some(obj) = last_json_object(text) {
        let mut out = Map::new();
        for name in &step.outputs {
            if let Some(v) = obj.get(name) {
                out.insert(name.clone(), v.clone());
            }
        }
        if !out.is_empty() {
            return out;
        }
    }
    let mut out = Map::new();
    out.insert(
        step.outputs[0].clone(),
        Value::String(text.trim().to_string()),
    );
    out
}

fn last_json_object(text: &str) -> Option<Map<String, Value>> {
    let text = text.trim();
    if let Ok(Value::Object(m)) = serde_json::from_str::<Value>(text) {
        return Some(m);
    }
    // The last ```json block, else the last {...} span.
    let candidates: Vec<&str> = text.rmatch_indices('{').map(|(i, _)| &text[i..]).collect();
    for start in candidates {
        if let Some(end) = start.rfind('}') {
            if let Ok(Value::Object(m)) = serde_json::from_str::<Value>(&start[..=end]) {
                return Some(m);
            }
        }
    }
    None
}

/// A workflow run as a tool call by the agent. Boxed and `Send` by
/// declaration: the agent loop awaits this and a step awaits the loop.
pub(crate) fn run_as_tool<'a>(
    spec: &'a AgentSpec,
    provider: &'a dyn Provider,
    call: &'a ToolCall,
    child: ChildRun,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ToolOutput> + Send + 'a>> {
    Box::pin(async move {
        let name = call
            .arguments
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let inputs = call
            .arguments
            .get("inputs")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        match run_workflow(spec, provider, name, &inputs, child).await {
            Ok(report) => {
                let mut out = error_output(report.text());
                out.is_error = !report.succeeded();
                out
            }
            Err(e) => error_output(format!("Workflow not run: {e}")),
        }
    })
}

/// Workflow inputs from `key=value` words (`key=<json>` when the value
/// parses as JSON).
pub fn parse_inputs(words: &[String]) -> Result<Map<String, Value>, String> {
    let mut out = Map::new();
    for w in words {
        let Some((k, v)) = w.split_once('=') else {
            return Err(format!("an input must be key=value, not {w:?}"));
        };
        let value =
            serde_json::from_str::<Value>(v).unwrap_or_else(|_| Value::String(v.to_string()));
        out.insert(k.trim().to_string(), value);
    }
    Ok(out)
}

/// Counts of steps by status, for logs.
pub fn summary_counts(report: &WorkflowReport) -> BTreeMap<&'static str, usize> {
    let mut m = BTreeMap::new();
    for s in &report.steps {
        let key = match s.status {
            StepStatus::Done => "done",
            StepStatus::Skipped => "skipped",
            StepStatus::Failed(_) => "failed",
        };
        *m.entry(key).or_insert(0) += 1;
    }
    m
}
