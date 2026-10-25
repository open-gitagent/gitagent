//! The `plan` tool: phases of tasks that run at the same time as
//! sub-agents, then one reconciling sub-agent that turns every result into
//! the answer. The lead agent writes the plan; this runs it.
//!
//! Each task is a sub-agent call (`subagent` with the task's prompt, plus
//! the earlier phases' results after the first phase). A phase's tasks run
//! `at_once` at a time; the next phase starts when the whole phase is done.
//! Events `plan` (phase_start, task_start, task_end, phase_end,
//! reconcile_start, reconcile_end) let the console and the page show the
//! lanes.

use gitagent_provider::ToolCall;
use gitagent_record::Events;
use gitagent_tools::ToolOutput;
use serde_json::{json, Map, Value};

use crate::{error_output, run_subagent, ChildRun, RunReport};

/// Most tasks one plan may hold, and most phases.
const MAX_TASKS: usize = 100;
const MAX_PHASES: usize = 10;

/// How each task's sub-agent is set up: the lead's settings, with its own
/// output directory under the plan's.
pub(crate) struct Children {
    pub template: ChildRun,
}

impl Children {
    fn child(&self, n: u32) -> ChildRun {
        let t = &self.template;
        ChildRun {
            cwd: t.cwd.clone(),
            out_dir: t.out_dir.join(format!("tasks/{n}")),
            remaining: t.remaining,
            sandbox: t.sandbox.clone(),
            hooks: t.hooks.clone(),
            mcp: t.mcp.clone(),
            approver: t.approver.clone(),
            kill: t.kill.clone(),
        }
    }
}

pub(crate) struct Done {
    pub out: ToolOutput,
    pub reports: Vec<RunReport>,
    pub tasks: u32,
    pub failed: u32,
}

struct Task {
    description: String,
    prompt: String,
    agent: Option<String>,
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

fn subagent_call(id: String, agent: Option<&str>, description: &str, prompt: String) -> ToolCall {
    let mut arguments = Map::new();
    arguments.insert("description".into(), json!(description));
    arguments.insert("prompt".into(), json!(prompt));
    if let Some(a) = agent {
        arguments.insert("agent".into(), json!(a));
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
        let mut phase_results = Vec::new();
        for batch in phase.tasks.chunks(at_once.max(1)) {
            let mut calls = Vec::new();
            for t in batch {
                seq += 1;
                let id = format!("plan-{}-{seq}", pi + 1);
                events.emit(
                    "plan",
                    json!({"action": "task_start", "phase": pi + 1, "id": id, "description": t.description, "agent": t.agent.clone().unwrap_or_else(|| gitagent_loader::GENERAL_AGENT.into())}),
                );
                calls.push(subagent_call(
                    id,
                    t.agent.as_deref(),
                    &t.description,
                    format!("{}{earlier}", t.prompt),
                ));
            }
            let futures: Vec<_> = calls
                .iter()
                .map(|c| run_subagent(spec, provider, c, children.child(seq_of(&c.id))))
                .collect();
            let outs = crate::join_all(futures).await;
            for (c, r) in calls.iter().zip(outs) {
                tasks_run += 1;
                let ok = !r.out.is_error;
                if !ok {
                    failed += 1;
                }
                if let Some(rep) = r.report {
                    reports.push(rep);
                }
                let desc = c.arguments["description"].as_str().unwrap_or("");
                events.emit(
                    "plan",
                    json!({"action": "task_end", "phase": pi + 1, "id": c.id, "description": desc, "ok": ok, "partial": r.partial, "result": preview(&r.answer)}),
                );
                phase_results.push(format!(
                    "### {desc}{}\n{}",
                    if ok { "" } else { " (failed)" },
                    r.answer.trim()
                ));
            }
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
        if failed > 0 {
            format!(", {failed} failed")
        } else {
            String::new()
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
    }
}

fn seq_of(id: &str) -> u32 {
    id.rsplit('-')
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}
