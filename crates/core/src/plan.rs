//! The `plan` tool: phases of tasks that run at the same time as
//! sub-agents, then one reconciling sub-agent that turns every result into
//! the answer. The lead agent writes the plan; this runs it.
//!
//! Each task is a sub-agent call (`subagent` with the task's prompt, plus
//! the earlier phases' results after the first phase). A phase's tasks run
//! `at_once` at a time; the next phase starts when the whole phase is done.
//! Events `plan` (phase_start, task_start, task_end, phase_end,
//! reconcile_start, reconcile_end) let the console draw the task board.

use gitagent_provider::ToolCall;
use gitagent_record::Events;
use gitagent_tools::ToolOutput;
use serde_json::{json, Map, Value};

use crate::{error_output, run_subagent, ChildRun, RunReport, SubagentRun};

/// Most tasks one plan may hold, and most phases.
const MAX_TASKS: usize = 100;
const MAX_PHASES: usize = 10;

/// How each task's sub-agent is set up: the lead's settings, with its own
/// output directory under the plan's.
pub(crate) struct Children {
    pub template: ChildRun,
    /// When the plan started: each task gets the time left from
    /// `template.remaining` minus what the plan has used so far.
    pub started: std::time::Instant,
}

impl Children {
    fn child(&self, n: u32) -> ChildRun {
        let t = &self.template;
        ChildRun {
            cwd: t.cwd.clone(),
            out_dir: t.out_dir.join(format!("tasks/{n}")),
            remaining: t
                .remaining
                .map(|r| r.saturating_sub(self.started.elapsed())),
            sandbox: t.sandbox.clone(),
            hooks: t.hooks.clone(),
            mcp: t.mcp.clone(),
            approver: t.approver.clone(),
            kill: t.kill.clone(),
        }
    }
}

/// One task of a phase: its call and how its sub-agent run went.
type Job<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = (ToolCall, SubagentRun)> + Send + 'a>>;

pub(crate) struct Done {
    pub out: ToolOutput,
    pub reports: Vec<RunReport>,
    pub tasks: u32,
    pub failed: u32,
    /// Tasks that ran out of turns and gave partial results.
    pub partial: u32,
}

struct Task {
    description: String,
    prompt: String,
    agent: Option<String>,
    /// The task's own turn limit, when the lead set one.
    max_turns: Option<u64>,
}

struct Phase {
    name: String,
    tasks: Vec<Task>,
}

fn parse(call: &ToolCall) -> Result<(Vec<Phase>, String, Option<String>), String> {
    let s = |v: &Value, k: &str| {
        v[k].as_str()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_string)
    };
    let phases_v = call
        .arguments
        .get("phases")
        .and_then(Value::as_array)
        .ok_or("`phases` must be a list")?;
    if phases_v.is_empty() {
        return Err("`phases` is empty".into());
    }
    if phases_v.len() > MAX_PHASES {
        return Err(format!("at most {MAX_PHASES} phases"));
    }
    let mut phases = Vec::new();
    let mut count = 0;
    for (i, p) in phases_v.iter().enumerate() {
        let tasks_v = p["tasks"]
            .as_array()
            .ok_or(format!("phase {} has no `tasks` list", i + 1))?;
        if tasks_v.is_empty() {
            return Err(format!("phase {} has no tasks", i + 1));
        }
        let mut tasks = Vec::new();
        for (j, t) in tasks_v.iter().enumerate() {
            let prompt = s(t, "prompt").ok_or(format!(
                "task {} of phase {} has no `prompt`",
                j + 1,
                i + 1
            ))?;
            tasks.push(Task {
                description: s(t, "description").unwrap_or_else(|| format!("task {}", j + 1)),
                prompt,
                agent: s(t, "agent"),
                max_turns: t["max_turns"].as_u64(),
            });
        }
        count += tasks.len();
        if count > MAX_TASKS {
            return Err(format!("at most {MAX_TASKS} tasks in a plan"));
        }
        phases.push(Phase {
            name: s(p, "name").unwrap_or_else(|| format!("phase {}", i + 1)),
            tasks,
        });
    }
    let reconcile = s(&Value::Object(call.arguments.clone()), "reconcile")
        .ok_or("`reconcile` is required: what to do with all the results")?;
    Ok((
        phases,
        reconcile,
        s(&Value::Object(call.arguments.clone()), "agent"),
    ))
}

fn subagent_call(
    id: String,
    agent: Option<&str>,
    description: &str,
    prompt: String,
    max_turns: Option<u64>,
) -> ToolCall {
    let mut arguments = Map::new();
    arguments.insert("description".into(), json!(description));
    arguments.insert("prompt".into(), json!(prompt));
    if let Some(a) = agent {
        arguments.insert("agent".into(), json!(a));
    }
    if let Some(n) = max_turns {
        arguments.insert("max_turns".into(), json!(n));
    }
    ToolCall {
        id,
        name: gitagent_tools::SUBAGENT.into(),
        arguments,
    }
}

fn preview(text: &str) -> String {
    let t: String = text.chars().take(160).collect();
    if t.len() < text.len() {
        format!("{t}…")
    } else {
        t
    }
}

pub(crate) async fn run(
    spec: &gitagent_loader::AgentSpec,
    provider: &dyn gitagent_provider::Provider,
    call: &ToolCall,
    children: &Children,
    events: &Events,
    at_once: usize,
) -> Done {
    let (phases, reconcile, reconcile_agent) = match parse(call) {
        Ok(p) => p,
        Err(e) => {
            return Done {
                out: error_output(format!("The plan is not usable: {e}.")),
                reports: Vec::new(),
                tasks: 0,
                failed: 0,
                partial: 0,
            }
        }
    };
    let goal = call
        .arguments
        .get("goal")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let n_phases = phases.len();
    let mut reports = Vec::new();
    let mut results: Vec<String> = Vec::new();
    let mut tasks_run: u32 = 0;
    let mut failed: u32 = 0;
    let mut partial: u32 = 0;
    let mut seq: u32 = 0;
    for (pi, phase) in phases.iter().enumerate() {
        events.emit(
            "plan",
            json!({"action": "phase_start", "phase": pi + 1, "of": n_phases, "name": phase.name, "tasks": phase.tasks.len()}),
        );
        let earlier = if results.is_empty() {
            String::new()
        } else {
            format!(
                "\n\n# Results of the earlier phases\n\n{}",
                results.join("\n\n")
            )
        };
        // Up to `at_once` tasks run at a time; the next starts as soon as
        // one finishes. Each reports its start and end as they happen.
        let mut jobs: Vec<Job<'_>> = Vec::new();
        for t in &phase.tasks {
            seq += 1;
            let id = format!("plan-{}-{seq}", pi + 1);
            let call = subagent_call(
                id,
                t.agent.as_deref(),
                &t.description,
                format!("{}{earlier}", t.prompt),
                t.max_turns,
            );
            let child = children.child(seq);
            let agent = t
                .agent
                .clone()
                .unwrap_or_else(|| gitagent_loader::GENERAL_AGENT.into());
            let description = t.description.clone();
            let phase_no = pi + 1;
            jobs.push(Box::pin(async move {
                events.emit(
                    "plan",
                    json!({"action": "task_start", "phase": phase_no, "id": call.id, "description": description, "agent": agent}),
                );
                let r = run_subagent(spec, provider, &call, child).await;
                events.emit(
                    "plan",
                    json!({"action": "task_end", "phase": phase_no, "id": call.id, "description": description, "ok": !r.out.is_error, "partial": r.partial, "result": preview(&r.answer)}),
                );
                (call, r)
            }));
        }
        let mut phase_results = Vec::new();
        for (c, r) in crate::join_limited(jobs, at_once.max(1)).await {
            tasks_run += 1;
            let ok = !r.out.is_error;
            if !ok {
                failed += 1;
            }
            if r.partial {
                partial += 1;
            }
            if let Some(rep) = r.report {
                reports.push(rep);
            }
            let desc = c.arguments["description"].as_str().unwrap_or("");
            phase_results.push(format!(
                "### {desc}{}\n{}",
                if !ok {
                    " (failed)"
                } else if r.partial {
                    " (stopped at its turn limit: partial results)"
                } else {
                    ""
                },
                r.answer.trim()
            ));
        }
        events.emit(
            "plan",
            json!({"action": "phase_end", "phase": pi + 1, "of": n_phases, "name": phase.name}),
        );
        results.extend(phase_results);
    }
    events.emit(
        "plan",
        json!({"action": "reconcile_start", "tasks": tasks_run}),
    );
    let prompt = format!(
        "{}{}\n\n# All results\n\n{}",
        if goal.is_empty() {
            String::new()
        } else {
            format!("Goal: {goal}\n\n")
        },
        reconcile,
        results.join("\n\n")
    );
    seq += 1;
    let rc = subagent_call(
        format!("plan-reconcile-{seq}"),
        reconcile_agent.as_deref(),
        "reconcile",
        prompt,
        None,
    );
    let r = run_subagent(spec, provider, &rc, children.child(seq)).await;
    if let Some(rep) = r.report {
        reports.push(rep);
    }
    events.emit(
        "plan",
        json!({"action": "reconcile_end", "ok": !r.out.is_error, "result": preview(&r.answer)}),
    );
    let header = format!(
        "{tasks_run} task{} in {n_phases} phase{} ran{}.",
        if tasks_run == 1 { "" } else { "s" },
        if n_phases == 1 { "" } else { "s" },
        match (failed, partial) {
            (0, 0) => String::new(),
            (f, 0) => format!(", {f} failed"),
            (0, p) => format!(
                ", {p} stopped at {} turn limit with partial results",
                if p == 1 { "its" } else { "their" }
            ),
            (f, p) => format!(", {f} failed, {p} stopped at their turn limit with partial results"),
        }
    );
    let out = if r.out.is_error {
        error_output(format!(
            "{header} The reconciling step failed: {}\n\nThe results, for you to combine:\n\n{}",
            r.answer.trim(),
            results.join("\n\n")
        ))
    } else {
        ToolOutput {
            is_error: false,
            ..error_output(format!("{header}\n\n{}", r.answer.trim()))
        }
    };
    Done {
        out,
        reports,
        tasks: tasks_run,
        failed,
        partial,
    }
}
