//! Integration tests: a scripted provider drives the real loop against a real
//! shell in a temporary directory.

use super::*;
use gitagent_loader::Masking;
use gitagent_loader::{load, LoadOptions};
use gitagent_provider::{Role, ScriptedProvider};

fn spec() -> AgentSpec {
    load(&LoadOptions {
        model: Some("scripted:test".into()),
        ..Default::default()
    })
    .unwrap()
}

struct Run {
    dir: tempfile::TempDir,
    report: RunReport,
    provider: ScriptedProvider,
}

impl Run {
    fn trajectory(&self) -> Value {
        serde_json::from_slice(&std::fs::read(&self.report.trajectory_path).unwrap()).unwrap()
    }

    fn counters(&self) -> Value {
        let path = self.dir.path().join("out/counters.json");
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }
}

async fn run_script(spec: &AgentSpec, instruction: &str, script: &str) -> Run {
    run_script_for(spec, instruction, script, 60).await
}

async fn run_script_for(spec: &AgentSpec, instruction: &str, script: &str, secs: u64) -> Run {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("work")).unwrap();
    let provider = ScriptedProvider::from_yaml(script).unwrap();
    let report = run(
        spec,
        &provider,
        RunOptions {
            instruction: instruction.into(),
            work_dir: dir.path().join("work"),
            out_dir: dir.path().join("out"),
            timeout: Some(Duration::from_secs(secs)),
            session_id: Some("test-session".into()),
            events: gitagent_record::Events::disabled(),
            resume: None,
            sandbox: None,
            tool_allowlist: None,
            tool_denylist: Vec::new(),
            hook_callbacks: None,
            mcp: None,
            approver: None,
            kill: None,
            steer: None,
            external_tools: None,
        },
    )
    .await
    .unwrap()
    .report;
    Run {
        dir,
        report,
        provider,
    }
}

#[tokio::test]
async fn a_tool_denylist_removes_tools_but_never_task_complete() {
    let spec = spec();
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("work")).unwrap();
    let provider = ScriptedProvider::from_yaml(
        "- tool_calls: [{name: task_complete, arguments: {summary: x}}]\n",
    )
    .unwrap();
    run(
        &spec,
        &provider,
        RunOptions {
            instruction: "x".into(),
            work_dir: dir.path().join("work"),
            out_dir: dir.path().join("out"),
            timeout: Some(Duration::from_secs(60)),
            session_id: None,
            events: gitagent_record::Events::disabled(),
            resume: None,
            sandbox: None,
            tool_allowlist: Some(vec![
                "exec".into(),
                "read_file".into(),
                "task_complete".into(),
            ]),
            tool_denylist: vec!["exec".into(), "task_complete".into()],
            hook_callbacks: None,
            mcp: None,
            approver: None,
            kill: None,
            steer: None,
            external_tools: None,
        },
    )
    .await
    .unwrap();
    let names: Vec<String> = provider.requests()[0]
        .tools
        .iter()
        .map(|t| t.name.clone())
        .collect();
    assert_eq!(names, vec!["read_file", "task_complete"]);
}

#[tokio::test]
async fn sdk_tool_naming_renames_tools_for_the_model_and_maps_calls_back() {
    let mut spec = spec();
    spec.profile.tool_naming = gitagent_loader::ToolNaming::Sdk;
    let script = r#"
- tool_calls:
    - name: Bash
      arguments: {command: "echo hi"}
- tool_calls:
    - name: Write
      arguments: {file_path: a.txt, content: "hello world\n"}
- tool_calls:
    - name: Edit
      arguments: {file_path: a.txt, old_string: world, new_string: there}
- tool_calls:
    - name: Read
      arguments: {file_path: a.txt}
- tool_calls:
    - name: Nope
      arguments: {}
- tool_calls:
    - name: task_complete
      arguments: {summary: done}
"#;
    let r = run_script(&spec, "x", script).await;
    assert_eq!(r.report.outcome, Outcome::Completed);
    let names: Vec<String> = r.provider.requests()[0]
        .tools
        .iter()
        .map(|t| t.name.clone())
        .collect();
    assert_eq!(
        names,
        vec![
            "Bash",
            "write_stdin",
            "Read",
            "Write",
            "Edit",
            "task_complete"
        ]
    );
    assert_eq!(
        std::fs::read_to_string(r.dir.path().join("work/a.txt")).unwrap(),
        "hello there\n"
    );
    let c = r.counters();
    assert_eq!(c["sdk_named_calls"], 4);
    assert_eq!(c["tool_calls"]["exec"], 1);
    assert_eq!(c["tool_calls"]["write_file"], 1);
    assert_eq!(c["tool_calls"]["str_replace"], 1);
    assert_eq!(c["tool_calls"]["read_file"], 1);
    // The trajectory keeps the names the model used; unknown names list `.claude/` names.
    let t = r.trajectory();
    assert_eq!(t["steps"][2]["tool_calls"][0]["function_name"], "Bash");
    let unknown = t["steps"][6]["observation"]["results"][0]["content"]
        .as_str()
        .unwrap();
    assert!(
        unknown.contains("Unknown tool `Nope`")
            && unknown.contains("Bash")
            && !unknown.contains("exec"),
        "{unknown}"
    );
    let read = t["steps"][5]["observation"]["results"][0]["content"]
        .as_str()
        .unwrap();
    assert!(read.contains("hello there"), "{read}");
}

fn agent(name: &str, max_turns: u32) -> gitagent_loader::AgentDef {
    gitagent_loader::AgentDef {
        name: name.into(),
        description: "Does a piece of work.".into(),
        tools: Some(vec!["exec".into()]),
        disallowed_tools: Vec::new(),
        max_turns,
        model: None,
        effort: None,
        instructions: String::new(),
        root: None,
        triggers: Vec::new(),
        delegation: None,
    }
}

#[tokio::test]
async fn subagents_in_one_reply_run_at_the_same_time() {
    let mut spec = spec();
    spec.profile.tools.subagents = true;
    spec.agents = vec![agent("worker", 5)];
    // Both children draw from the same script: each runs until its first
    // task_complete, so the order they interleave in does not matter.
    let script = r#"
- tool_calls:
    - name: subagent
      arguments: {agent: worker, description: first, prompt: "do a"}
    - name: subagent
      arguments: {agent: worker, description: second, prompt: "do b"}
- tool_calls: [{name: exec, arguments: {cmd: echo one}}]
- tool_calls: [{name: task_complete, arguments: {summary: child done}}]
- tool_calls: [{name: exec, arguments: {cmd: echo two}}]
- tool_calls: [{name: task_complete, arguments: {summary: child done}}]
- tool_calls: [{name: task_complete, arguments: {summary: parent done}}]
"#;
    let r = run_script_for(&spec, "x", script, 600).await;
    assert_eq!(r.report.outcome, Outcome::Completed);
    let c = r.counters();
    assert_eq!(c["subagent_calls"], 2);
    assert_eq!(c["subagent_parallel_turns"], 1);
    assert_eq!(c["subagent_turns"], 4);
    assert!(r
        .dir
        .path()
        .join("out/subagents/1/trajectory.json")
        .is_file());
    assert!(r
        .dir
        .path()
        .join("out/subagents/2/trajectory.json")
        .is_file());
    let t = r.trajectory();
    let results = &t["steps"][2]["observation"]["results"];
    assert_eq!(results.as_array().unwrap().len(), 2);
    for res in results.as_array().unwrap() {
        assert!(res["content"].as_str().unwrap().contains("child done"));
    }
}

#[tokio::test]
async fn a_subagent_can_use_its_own_model_and_effort() {
    let mut spec = spec();
    spec.profile.tools.subagents = true;
    let dir = tempfile::tempdir().unwrap();
    let child_script = dir.path().join("child.yaml");
    std::fs::write(
        &child_script,
        "- tool_calls: [{name: task_complete, arguments: {summary: via its own model}}]\n",
    )
    .unwrap();
    let mut def = agent("specialist", 5);
    def.model = Some(format!("scripted:{}", child_script.display()));
    def.effort = Some("low".into());
    let mut wrong = agent("elsewhere", 5);
    wrong.model = Some("openai:gpt-x".into());
    spec.agents = vec![wrong, def];
    let script = r#"
- tool_calls:
    - name: subagent
      arguments: {agent: specialist, description: s, prompt: "go"}
    - name: subagent
      arguments: {agent: elsewhere, description: e, prompt: "go"}
- tool_calls: [{name: task_complete, arguments: {summary: parent done}}]
"#;
    let r = run_script_for(&spec, "x", script, 600).await;
    assert_eq!(r.report.outcome, Outcome::Completed);
    let c = r.counters();
    assert_eq!(c["subagent_model_overrides"], 1);
    assert_eq!(c["subagent_effort_overrides"], 1);
    let t = r.trajectory();
    let results = t["steps"][2]["observation"]["results"]
        .as_array()
        .unwrap()
        .clone();
    assert!(results[0]["content"]
        .as_str()
        .unwrap()
        .contains("via its own model"));
    assert!(results[1]["content"]
        .as_str()
        .unwrap()
        .contains("parent's provider"));
    // The parent's script was not consumed by the child.
    assert_eq!(r.provider.requests().len(), 2);
    let child: Value = serde_json::from_slice(
        &std::fs::read(r.dir.path().join("out/subagents/1/trajectory.json")).unwrap(),
    )
    .unwrap();
    assert!(child["agent"]["model_name"]
        .as_str()
        .unwrap()
        .ends_with("child.yaml"));
}

#[tokio::test]
async fn a_subagent_out_of_turns_returns_a_partial_result() {
    let mut spec = spec();
    spec.profile.tools.subagents = true;
    spec.agents = vec![agent("worker", 1)];
    let script = r#"
- tool_calls:
    - name: subagent
      arguments: {agent: worker, description: w, prompt: "go"}
- text: Halfway there.
  tool_calls: [{name: exec, arguments: {cmd: echo partial}}]
- tool_calls: [{name: task_complete, arguments: {summary: parent done}}]
"#;
    let r = run_script_for(&spec, "x", script, 600).await;
    assert_eq!(r.report.outcome, Outcome::Completed);
    assert_eq!(r.counters()["subagent_partial"], 1);
    let t = r.trajectory();
    let res = &t["steps"][2]["observation"]["results"][0];
    let content = res["content"].as_str().unwrap();
    assert!(
        content.contains("partial") && content.contains("Halfway there."),
        "{content}"
    );
    // A partial result is not an error for the parent.
    // The parent's second request is the last one made (the child's sits between).
    let requests = r.provider.requests();
    let last = requests.last().unwrap().messages.last().unwrap().clone();
    assert!(matches!(
        &last.content[0],
        gitagent_provider::Content::ToolResult {
            is_error: false,
            ..
        }
    ));
}

const ECHO_THEN_COMPLETE: &str = "
- text: Checking the shell.
  tool_calls:
    - name: exec
      arguments: {cmd: echo hi}
- tool_calls:
    - name: task_complete
      arguments: {summary: printed hi}
";

#[tokio::test]
async fn echo_then_complete() {
    let r = run_script(&spec(), "Print hi.", ECHO_THEN_COMPLETE).await;
    assert_eq!(r.report.outcome, Outcome::Completed);
    assert_eq!(r.report.turns, 2);

    let t = r.trajectory();
    gitagent_record::atif::validate(&serde_json::from_value(t.clone()).unwrap()).unwrap();
    let steps = t["steps"].as_array().unwrap();
    assert_eq!(steps[0]["source"], "system");
    assert_eq!(steps[1]["source"], "user");
    assert!(steps[1]["message"].as_str().unwrap().contains("Print hi."));
    assert_eq!(steps[2]["tool_calls"][0]["function_name"], "exec");
    let obs = steps[2]["observation"]["results"][0]["content"]
        .as_str()
        .unwrap();
    assert!(obs.starts_with("hi\n[exit 0"), "{obs}");
    assert_eq!(steps[3]["tool_calls"][0]["function_name"], "task_complete");
    assert_eq!(t["extra"]["outcome"], "completed");
    assert!(t["final_metrics"]["total_prompt_tokens"].as_u64().unwrap() > 0);

    let c = r.counters();
    assert_eq!(c["turns"], 2);
    assert_eq!(c["tool_calls"]["exec"], 1);
    assert_eq!(c["tool_calls"]["task_complete"], 1);
    assert!(c["input_tokens"].as_u64().unwrap() > 0);

    // The model saw the command output as a tool result on its second turn.
    let reqs = r.provider.requests();
    let last = reqs[1].messages.last().unwrap();
    assert!(
        matches!(&last.content[0], Content::ToolResult { content, .. } if content.starts_with("hi\n"))
    );
}

#[tokio::test]
async fn system_prompt_and_tools_are_identical_across_tasks() {
    let spec = spec();
    let a = run_script(&spec, "Task A", ECHO_THEN_COMPLETE).await;
    let b = run_script(&spec, "A different task B", ECHO_THEN_COMPLETE).await;
    let (ra, rb) = (&a.provider.requests()[0], &b.provider.requests()[0]);
    assert_eq!(ra.system, rb.system);
    assert_eq!(
        serde_json::to_string(&ra.tools).unwrap(),
        serde_json::to_string(&rb.tools).unwrap()
    );
    assert!(!ra.system.contains("Task A"));
}

#[tokio::test]
async fn stops_at_max_turns() {
    let mut spec = spec();
    spec.config.max_turns = 2;
    let script = "
- tool_calls: [{name: exec, arguments: {cmd: 'true'}}]
- tool_calls: [{name: exec, arguments: {cmd: 'true'}}]
- tool_calls: [{name: exec, arguments: {cmd: 'true'}}]
";
    let r = run_script(&spec, "loop", script).await;
    assert_eq!(r.report.outcome, Outcome::MaxTurns);
    assert_eq!(r.report.turns, 2);
}

#[tokio::test]
async fn nudges_then_gives_up_without_tool_calls() {
    let script = "
- text: thinking
- text: still thinking
- text: done?
";
    let r = run_script(&spec(), "x", script).await;
    assert_eq!(r.report.outcome, Outcome::NoToolCalls);
    assert_eq!(r.counters()["no_tool_turns"], 3);
    let reqs = r.provider.requests();
    let nudge = reqs[1].messages.last().unwrap();
    assert_eq!(nudge.role, Role::User);
}

#[tokio::test]
async fn bad_arguments_and_unknown_tools_are_reported_to_the_model() {
    let script = "
- tool_calls:
    - name: exec
      arguments: {command: ls}
    - name: browse
      arguments: {}
- tool_calls: [{name: task_complete}]
";
    let r = run_script(&spec(), "x", script).await;
    assert_eq!(r.report.outcome, Outcome::Completed);
    let reqs = r.provider.requests();
    let results = &reqs[1].messages.last().unwrap().content;
    assert!(
        matches!(&results[0], Content::ToolResult { content, is_error: true, .. } if content.contains("missing required argument `cmd`"))
    );
    assert!(
        matches!(&results[1], Content::ToolResult { content, is_error: true, .. } if content.contains("Unknown tool"))
    );
    // Both the bad arguments and the unknown tool count as malformed.
    assert_eq!(r.counters()["malformed_calls"], 2);
    assert!(
        matches!(&results[0], Content::ToolResult { content, .. } if content.contains("Expected arguments: background (boolean), cmd (string, required), yield_ms (integer)."))
    );
}

#[tokio::test]
async fn provider_failure_ends_the_run_with_a_record() {
    let r = run_script(
        &spec(),
        "x",
        "- tool_calls: [{name: exec, arguments: {cmd: 'true'}}]\n",
    )
    .await;
    assert!(matches!(r.report.outcome, Outcome::ProviderFailed(_)));
    assert!(r.trajectory()["extra"]["outcome"]
        .as_str()
        .unwrap()
        .starts_with("provider_failed"));
}

#[tokio::test]
async fn shell_does_not_see_api_keys() {
    // The shell inherits this process's environment except for secrets.
    std::env::set_var("GITAGENT_API_KEY", "secret-value");
    let script = "
- tool_calls: [{name: exec, arguments: {cmd: 'echo key=${GITAGENT_API_KEY:-none}'}}]
- tool_calls: [{name: task_complete}]
";
    let r = run_script(&spec(), "x", script).await;
    let reqs = r.provider.requests();
    let result = &reqs[1].messages.last().unwrap().content[0];
    assert!(
        matches!(result, Content::ToolResult { content, .. } if content.starts_with("key=none")),
        "{result:?}"
    );
}

#[tokio::test]
async fn refusal_ends_the_run() {
    let script = "
- tool_calls: [{name: exec, arguments: {cmd: 'true'}}]
- refusal: true
";
    let r = run_script(&spec(), "x", script).await;
    assert_eq!(r.report.outcome, Outcome::Refused);
    assert_eq!(r.counters()["refusals"], 1);
    assert_eq!(r.trajectory()["extra"]["outcome"], "refused");
}

#[tokio::test]
async fn cache_breakpoints_come_from_the_profile() {
    let spec = load(&LoadOptions {
        model: Some("scripted:test".into()),
        profile: Some("anthropic".into()),
        ..Default::default()
    })
    .unwrap();
    let r = run_script(&spec, "x", ECHO_THEN_COMPLETE).await;
    let req = &r.provider.requests()[0];
    assert!(req.cache.system);
    assert_eq!(req.cache.last_messages, 2);
    assert_eq!(req.effort, spec.profile.effort);
    assert_eq!(req.temperature, None);
}

#[tokio::test]
async fn background_session_and_write_stdin_through_the_loop() {
    let script = r#"
- tool_calls: [{name: exec, arguments: {cmd: "cat -n", background: true, yield_ms: 300}}]
- tool_calls: [{name: write_stdin, arguments: {session_id: 2, chars: 'hello\n', yield_ms: 1000}}]
- tool_calls: [{name: write_stdin, arguments: {session_id: 2, chars: '\x04', yield_ms: 3000}}]
- tool_calls: [{name: task_complete}]
"#;
    let r = run_script(&spec(), "x", script).await;
    assert_eq!(r.report.outcome, Outcome::Completed);
    let reqs = r.provider.requests();
    let result = |turn: usize| match &reqs[turn].messages.last().unwrap().content[0] {
        Content::ToolResult { content, .. } => content.clone(),
        other => panic!("{other:?}"),
    };
    assert!(
        result(1).contains("session 2 still running"),
        "{}",
        result(1)
    );
    assert!(result(2).contains("1\thello"), "{}", result(2));
    assert!(result(3).contains("[session 2 · exit 0"), "{}", result(3));
    let c = r.counters();
    assert_eq!(c["background_sessions"], 1);
    assert_eq!(c["stdin_writes"], 2);
    assert_eq!(c["tool_calls"]["write_stdin"], 2);
}

#[tokio::test]
async fn write_stdin_can_be_switched_off_in_the_profile() {
    let mut spec = spec();
    spec.profile.tools.write_stdin = false;
    let script = "
- tool_calls: [{name: write_stdin, arguments: {session_id: 1, chars: 'x'}}]
- tool_calls: [{name: task_complete}]
";
    let r = run_script(&spec, "x", script).await;
    let reqs = r.provider.requests();
    assert!(reqs[0].tools.iter().all(|t| t.name != "write_stdin"));
    assert!(
        matches!(&reqs[1].messages.last().unwrap().content[0], Content::ToolResult { content, is_error: true, .. } if content.contains("Unknown tool"))
    );
}

#[tokio::test]
async fn tool_output_is_shaped_and_spilled() {
    let script = r#"
- tool_calls: [{name: exec, arguments: {cmd: "for i in $(seq 1 40); do echo \"item $i ok\"; done"}}]
- tool_calls: [{name: task_complete}]
"#;
    let r = run_script(&spec(), "x", script).await;
    let reqs = r.provider.requests();
    let Content::ToolResult { content, .. } = &reqs[1].messages.last().unwrap().content[0] else {
        panic!("expected a tool result");
    };
    assert!(
        content.starts_with("item 1 ok\n[38 similar lines hidden]\nitem 40 ok\n"),
        "{content}"
    );
    assert!(content.contains("40 lines, full output: "), "{content}");
    let spill = r.dir.path().join("out/out/1.log");
    assert_eq!(std::fs::read_to_string(spill).unwrap().lines().count(), 40);
    let c = r.counters();
    assert_eq!(c["collapse_events"], 1);
    assert!(c["shaped_bytes_in"].as_u64().unwrap() > c["shaped_bytes_out"].as_u64().unwrap());
}

#[tokio::test]
async fn write_file_and_image_read_through_the_loop() {
    let script = r#"
- tool_calls:
    - {name: write_file, arguments: {path: notes/plan.txt, content: "step one\n"}}
    - {name: exec, arguments: {cmd: "printf '\\211PNG\\r\\n\\032\\nDATA' > pic.png"}}
- tool_calls: [{name: image_read, arguments: {path: pic.png}}]
- tool_calls: [{name: task_complete}]
"#;
    let mut spec = spec();
    spec.profile.tools.image_read = true;
    let r = run_script(&spec, "x", script).await;
    assert_eq!(r.report.outcome, Outcome::Completed);
    let work = r.dir.path().join("work");
    assert_eq!(
        std::fs::read_to_string(work.join("notes/plan.txt")).unwrap(),
        "step one\n"
    );
    let reqs = r.provider.requests();
    let Content::ToolResult {
        images, content, ..
    } = &reqs[2].messages.last().unwrap().content[0]
    else {
        panic!("expected a tool result");
    };
    assert!(content.contains("image/png"), "{content}");
    assert_eq!(images.len(), 1);
    assert_eq!(images[0].media_type, "image/png");
}

#[tokio::test]
async fn text_fallback_recovers_calls_written_as_text() {
    let script = r#"
- text: "Let me look.\n<tool_call>{\"name\": \"exec\", \"arguments\": {\"cmd\": \"echo recovered\"}}</tool_call>"
- text: '```json\n{"name": "task_complete", "arguments": {}}\n```'
"#;
    let mut spec = spec();
    spec.profile.tool_calling = gitagent_loader::ToolCalling::NativeWithTextFallback;
    let r = run_script(&spec, "x", script).await;
    assert_eq!(r.report.outcome, Outcome::Completed);
    let reqs = r.provider.requests();
    // The recovered call was run and its result sent back as a tool result.
    assert!(matches!(
        &reqs[1].messages.last().unwrap().content[0],
        Content::ToolResult { content, .. } if content.starts_with("recovered\n")
    ));
    assert_eq!(r.counters()["repairs"], 2);

    // With native-only calling the same text is just text.
    let mut native = spec.clone();
    native.profile.tool_calling = gitagent_loader::ToolCalling::Native;
    let r = run_script(&native, "x", script).await;
    let c = r.counters();
    assert_eq!(c["no_tool_turns"], 2);
    assert!(c.get("repairs").is_none());
}

fn with_stop_check(mut spec: AgentSpec) -> AgentSpec {
    spec.profile.stop.enabled = true;
    spec
}

#[tokio::test]
async fn stop_check_rejects_unverified_completion_twice_then_accepts() {
    let script = r#"
- tool_calls: [{name: write_file, arguments: {path: app.py, content: "print(1)\n"}}]
- tool_calls: [{name: task_complete}]
- tool_calls: [{name: exec, arguments: {cmd: "cat app.py"}}, {name: task_complete}]
- tool_calls: [{name: task_complete}]
"#;
    let r = run_script(&with_stop_check(spec()), "Write app.py printing 1.", script).await;
    assert_eq!(r.report.outcome, Outcome::Completed);
    let reqs = r.provider.requests();
    let Content::ToolResult { content, .. } = &reqs[2].messages.last().unwrap().content[0] else {
        panic!("expected a tool result");
    };
    assert!(content.starts_with("Not marked complete: nothing was verified after your last change (`write_file app.py`)"), "{content}");
    assert!(content.ends_with("Original task:\nWrite app.py printing 1."));
    let c = r.counters();
    assert_eq!(c["stop_rejections"], 2);
    assert_eq!(c["stop_accepts"], 1);
    assert_eq!(r.report.turns, 4);
}

#[tokio::test]
async fn stop_check_accepts_after_verification() {
    let script = r#"
- tool_calls: [{name: exec, arguments: {cmd: "echo 'print(6*7)' > app.py"}}]
- tool_calls: [{name: exec, arguments: {cmd: "python3 app.py"}}]
- tool_calls: [{name: task_complete}]
"#;
    let r = run_script(&with_stop_check(spec()), "x", script).await;
    assert_eq!(r.report.outcome, Outcome::Completed);
    let c = r.counters();
    assert_eq!(c["stop_accepts"], 1);
    assert!(c.get("stop_rejections").is_none());
}

#[tokio::test]
async fn snapshot_goes_into_the_first_message() {
    let mut spec = spec();
    spec.profile.snapshot = true;
    let r = run_script(&spec, "Do the thing.", ECHO_THEN_COMPLETE).await;
    let first = &r.provider.requests()[0].messages[0];
    let Content::Text { text } = &first.content[0] else {
        panic!("expected text");
    };
    assert!(
        text.starts_with("# Task\n\nDo the thing.\n\n# Environment snapshot\n"),
        "{text}"
    );
    assert!(text.contains("directory: "));
    assert_eq!(r.counters()["snapshot_ok"], 1);
    // The system prompt is unchanged by the snapshot.
    assert_eq!(r.provider.requests()[0].system, spec.system_prompt);
}

#[test]
fn built_in_stop_patterns_compile() {
    for model in ["anthropic:m", "openai:m", "google:m", "other:m"] {
        let spec = load(&LoadOptions {
            model: Some(model.into()),
            ..Default::default()
        })
        .unwrap();
        let s = &spec.profile.stop;
        StopCheck::new(&s.edit_patterns, &s.verify_patterns, s.max_rejections).unwrap();
        assert!(!s.enabled, "the stop check stays off until measured");
        assert!(
            !spec.profile.snapshot,
            "the snapshot stays off until measured"
        );
    }
}

#[test]
fn built_in_patterns_classify_common_commands() {
    let spec = spec();
    let s = &spec.profile.stop;
    let verdict = |cmds: &[(&str, bool)]| {
        let mut c = StopCheck::new(&s.edit_patterns, &s.verify_patterns, 2).unwrap();
        for (i, (cmd, finished)) in cmds.iter().enumerate() {
            c.observe_command(i as u64, cmd, *finished);
        }
        c.on_complete()
    };
    let rejected = |cmds: &[(&str, bool)]| matches!(verdict(cmds), StopVerdict::Reject { .. });
    assert!(rejected(&[(
        "cat > main.c <<'EOF'\nint main(){}\nEOF",
        true
    )]));
    assert!(!rejected(&[
        ("cat > main.c <<'EOF'\nint main(){}\nEOF", true),
        ("gcc main.c -o m && ./m", true)
    ]));
    assert!(rejected(&[("pip install requests", true)]));
    assert!(!rejected(&[
        ("pip install requests", true),
        ("curl -s localhost:8000", true)
    ]));
    assert!(rejected(&[("git checkout -b fix", true), ("ls", true)]));
    assert!(!rejected(&[
        ("git merge topic", true),
        ("git log --oneline -3", true)
    ]));
    assert!(!rejected(&[(
        "ls -la; cat README.md; grep -r foo . 2>/dev/null",
        true
    )]));
    assert!(rejected(&[
        ("sed -i 's/a/b/' x.py", true),
        ("pytest", false)
    ]));
}

fn result_text(req: &gitagent_provider::ChatRequest) -> String {
    req.messages
        .last()
        .unwrap()
        .content
        .iter()
        .map(|c| match c {
            Content::ToolResult { content, .. } => content.clone(),
            Content::Text { text } => format!("NOTE: {text}"),
            _ => String::new(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn loop_detector_warns_and_bumps_effort() {
    let script = "
- tool_calls: [{name: exec, arguments: {cmd: 'false'}}]
- tool_calls: [{name: exec, arguments: {cmd: 'false'}}]
- tool_calls: [{name: exec, arguments: {cmd: 'false'}}]
- tool_calls: [{name: task_complete}]
";
    let mut spec = spec();
    spec.profile.guards.loop_detector = true;
    spec.profile.guards.loop_effort_bump = true;
    let r = run_script(&spec, "x", script).await;
    let reqs = r.provider.requests();
    assert!(!result_text(&reqs[2]).contains("NOTE"));
    assert!(result_text(&reqs[3]).ends_with(&format!("NOTE: {LOOP_MESSAGE}")));
    assert_eq!(reqs[3].effort, spec.profile.effort.next());
    assert_eq!(reqs[2].effort, spec.profile.effort);
    let c = r.counters();
    assert_eq!(c["loop_triggers"], 1);
    assert_eq!(c["effort_bumps"], 1);
}

#[tokio::test]
async fn time_budget_footer_nudge_and_final_call() {
    let script = "
- tool_calls: [{name: exec, arguments: {cmd: 'true'}}]
- tool_calls: [{name: task_complete}]
";
    let mut spec = spec();
    spec.profile.guards.time_budget = true;
    spec.profile.guards.nudge_at = 0.0;
    spec.profile.guards.force_stop_at = 2.0;
    let r = run_script(&spec, "x", script).await;
    let text = result_text(&r.provider.requests()[1]);
    assert!(text.contains("min left]"), "{text}");
    assert!(text.contains("NOTE: Time check: about"), "{text}");
    assert_eq!(r.counters()["nudges"], 1);

    // Once the final call is made, the stop check no longer rejects.
    let script = "
- tool_calls: [{name: write_file, arguments: {path: a.txt, content: x}}]
- tool_calls: [{name: task_complete}]
";
    let mut spec = with_stop_check(spec);
    spec.profile.guards.force_stop_at = 0.0;
    let r = run_script(&spec, "x", script).await;
    assert_eq!(r.report.outcome, Outcome::Completed);
    let text = result_text(&r.provider.requests()[1]);
    assert!(text.contains("NOTE: Time is almost up"), "{text}");
    let c = r.counters();
    assert_eq!(c["forced_stops"], 1);
    assert!(c.get("stop_rejections").is_none());
}

#[tokio::test]
async fn client_masking_hides_old_results_in_batches() {
    let script = "
- tool_calls: [{name: exec, arguments: {cmd: 'seq 1000 1300'}}]
- tool_calls: [{name: exec, arguments: {cmd: 'seq 2000 2300'}}]
- tool_calls: [{name: exec, arguments: {cmd: 'seq 3000 3300'}}]
- tool_calls: [{name: task_complete}]
";
    let mut spec = spec();
    spec.profile.shape.collapse_min_run = 0;
    spec.profile.context.masking = Masking::Client;
    spec.profile.context.mask_batch_tokens = 1;
    spec.profile.context.keep_last = 1;
    let r = run_script(&spec, "x", script).await;
    let reqs = r.provider.requests();
    let results: Vec<String> = reqs[3]
        .messages
        .iter()
        .flat_map(|m| &m.content)
        .filter_map(|c| match c {
            Content::ToolResult { content, .. } => Some(content.clone()),
            _ => None,
        })
        .collect();
    assert!(
        results[0].starts_with(gitagent_context::MASK_PREFIX),
        "{}",
        results[0]
    );
    assert!(results[0].contains("status was exit 0, 301 lines; full output: "));
    assert!(results[1].starts_with(gitagent_context::MASK_PREFIX));
    assert!(results[2].starts_with("3000\n"));
    assert!(r.counters()["masks_applied"].as_u64().unwrap() >= 2);
}

#[tokio::test]
async fn compaction_near_the_window_replaces_the_history() {
    let script = "
- tool_calls: [{name: exec, arguments: {cmd: 'seq 1 2000'}}]
- text: \"DONE: printed numbers\\nPENDING: finish\"
- tool_calls: [{name: task_complete}]
";
    let mut spec = spec();
    spec.profile.shape.collapse_min_run = 0;
    spec.profile.context_window = 2500;
    spec.profile.context.compact_at_tokens = 0;
    let r = run_script(&spec, "Count.", script).await;
    assert_eq!(r.report.outcome, Outcome::Completed);
    let reqs = r.provider.requests();
    assert!(result_text(&reqs[1]).ends_with(&format!("NOTE: {COMPACTION_REQUEST}")));
    let after = &reqs[2].messages;
    assert_eq!(after.len(), 1);
    let Content::Text { text } = &after[0].content[0] else {
        panic!("expected text");
    };
    assert!(
        text.starts_with("# Task\n\nCount.\n# Progress so far"),
        "{text}"
    );
    assert!(text.ends_with("DONE: printed numbers\nPENDING: finish\n"));
    assert_eq!(r.counters()["compactions"], 1);
}

#[tokio::test]
async fn context_overflow_triggers_compaction_and_a_retry() {
    let script = "
- tool_calls: [{name: exec, arguments: {cmd: 'echo step one'}}]
- error: context_overflow
- error: fatal
- tool_calls: [{name: task_complete}]
";
    let r = run_script(&spec(), "x", script).await;
    assert_eq!(r.report.outcome, Outcome::Completed);
    let c = r.counters();
    assert_eq!(c["overflow_compactions"], 1);
    assert_eq!(c["compactions"], 1);
    // The summary request failed, so the summary was built from the calls.
    assert_eq!(c["compaction_fallbacks"], 1);
    let Content::Text { text } = &r.provider.requests()[3].messages[0].content[0] else {
        panic!("expected text");
    };
    assert!(text.contains("RECENT ACTIONS"), "{text}");
    assert!(text.contains("echo step one"));
}

#[tokio::test]
async fn server_masking_and_one_hour_cache_reach_the_request() {
    let mut spec = spec();
    spec.profile.context.masking = Masking::Server;
    spec.profile.cache.ttl = "1h".into();
    let r = run_script(&spec, "x", ECHO_THEN_COMPLETE).await;
    let req = &r.provider.requests()[0];
    let m = req.server_masking.expect("server masking");
    assert_eq!(m.trigger_tokens, spec.profile.context.mask_batch_tokens);
    assert_eq!(m.keep, spec.profile.context.keep_last);
    assert_eq!(m.clear_at_least, spec.profile.context.mask_batch_tokens / 2);
    assert!(req.cache.one_hour);
}

#[tokio::test]
async fn edit_tools_follow_the_profile_and_count_as_changes() {
    let script = r#"
- tool_calls: [{name: write_file, arguments: {path: app.py, content: "print('hi')\n"}}]
- tool_calls: [{name: str_replace, arguments: {path: app.py, old_str: "'hi'", new_str: "'hello'"}}]
- tool_calls: [{name: task_complete}]
- tool_calls: [{name: exec, arguments: {cmd: "python3 app.py"}}]
- tool_calls: [{name: task_complete}]
"#;
    let mut checked = with_stop_check(spec());
    checked.profile.edit_tool = gitagent_loader::EditTool::StrReplace;
    let r = run_script(&checked, "x", script).await;
    assert_eq!(r.report.outcome, Outcome::Completed);
    let work = r.dir.path().join("work");
    assert_eq!(
        std::fs::read_to_string(work.join("app.py")).unwrap(),
        "print('hello')\n"
    );
    let reqs = r.provider.requests();
    assert!(result_text(&reqs[3]).contains("(`str_replace app.py`)"));
    assert_eq!(r.counters()["stop_rejections"], 1);

    let script = "
- tool_calls:
    - name: apply_patch
      arguments:
        patch: \"*** Begin Patch\\n*** Add File: notes.txt\\n+one\\n*** End Patch\"
- tool_calls: [{name: task_complete}]
";
    let mut patched = spec();
    patched.profile.edit_tool = gitagent_loader::EditTool::ApplyPatch;
    let r = run_script(&patched, "x", script).await;
    assert!(r.provider.requests()[0]
        .tools
        .iter()
        .any(|t| t.name == "apply_patch"));
    assert!(r.provider.requests()[0]
        .tools
        .iter()
        .all(|t| t.name != "str_replace"));
    assert_eq!(
        std::fs::read_to_string(r.dir.path().join("work/notes.txt")).unwrap(),
        "one\n"
    );
}

#[tokio::test]
async fn events_describe_the_run_as_json_lines() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("work")).unwrap();
    let provider = ScriptedProvider::from_yaml(ECHO_THEN_COMPLETE).unwrap();
    let buf = gitagent_record::SharedBuffer::default();
    let finished = run(
        &spec(),
        &provider,
        RunOptions {
            instruction: "Print hi.".into(),
            work_dir: dir.path().join("work"),
            out_dir: dir.path().join("out"),
            timeout: Some(Duration::from_secs(60)),
            session_id: None,
            events: gitagent_record::Events::to_writer(Box::new(buf.clone())),
            resume: None,
            sandbox: None,
            tool_allowlist: None,
            tool_denylist: Vec::new(),
            hook_callbacks: None,
            mcp: None,
            approver: None,
            kill: None,
            steer: None,
            external_tools: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(finished.report.summary.as_deref(), Some("printed hi"));
    let lines = buf.lines();
    let kinds: Vec<&str> = lines.iter().map(|l| l["event"].as_str().unwrap()).collect();
    assert_eq!(
        kinds,
        [
            "turn_start",
            "model_text",
            "tool_call",
            "tool_result",
            "turn_end",
            "turn_start",
            "model_text",
            "tool_call",
            "tool_result",
            "turn_end",
            "run_end"
        ]
    );
    assert_eq!(lines[2]["arguments"]["cmd"], "echo hi");
    assert!(lines[3]["content"].as_str().unwrap().starts_with("hi\n"));
    let end = lines.last().unwrap();
    assert_eq!(end["outcome"], "completed");
    assert_eq!(end["summary"], "printed hi");
    assert_eq!(end["counters"]["tool_calls"]["exec"], 1);
}

#[tokio::test]
async fn a_follow_up_task_continues_the_conversation_and_shell() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("work")).unwrap();
    let provider = ScriptedProvider::from_yaml(
        "
- tool_calls: [{name: exec, arguments: {cmd: 'export GA_NOTE=kept; echo first'}}]
- tool_calls: [{name: task_complete, arguments: {summary: one}}]
- tool_calls: [{name: exec, arguments: {cmd: 'echo $GA_NOTE'}}]
- tool_calls: [{name: task_complete, arguments: {summary: two}}]
",
    )
    .unwrap();
    let opts = |instruction: &str, out: &str, resume| RunOptions {
        instruction: instruction.into(),
        work_dir: dir.path().join("work"),
        out_dir: dir.path().join(out),
        timeout: Some(Duration::from_secs(60)),
        session_id: None,
        events: gitagent_record::Events::disabled(),
        resume,
        sandbox: None,
        tool_allowlist: None,
        tool_denylist: Vec::new(),
        hook_callbacks: None,
        mcp: None,
        approver: None,
        kill: None,
        steer: None,
        external_tools: None,
    };
    let first = run(&spec(), &provider, opts("First task.", "t1", None))
        .await
        .unwrap();
    assert_eq!(first.report.summary.as_deref(), Some("one"));
    let second = run(
        &spec(),
        &provider,
        opts("Second task.", "t2", Some(first.into_resume())),
    )
    .await
    .unwrap();
    assert_eq!(second.report.summary.as_deref(), Some("two"));
    let reqs = provider.requests();
    // The second task's first request carries the whole first conversation.
    let follow_up = &reqs[2].messages;
    assert_eq!(follow_up.len(), 5);
    assert!(
        matches!(&follow_up[4].content.last().unwrap(), Content::Text { text } if text == "# Next task\n\nSecond task.\n")
    );
    // The shell kept its state.
    assert!(
        result_text(&reqs[3]).starts_with("kept\n"),
        "{}",
        result_text(&reqs[3])
    );
}

#[tokio::test]
async fn hooks_can_block_and_rewrite_calls() {
    let cfg = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(cfg.path().join("hooks")).unwrap();
    std::fs::write(
        cfg.path().join("hooks/pre_tool_use"),
        r#"input=$(cat); case "$input" in *forbidden*) echo '{"decision":"block","reason":"not allowed here"}';; esac"#,
    )
    .unwrap();
    std::fs::write(
        cfg.path().join("hooks/post_tool_use"),
        r#"input=$(cat); case "$input" in *SECRET*) echo '{"decision":"modify","output":"[redacted]"}';; esac"#,
    )
    .unwrap();
    let mut spec = spec();
    spec.config_dirs = vec![cfg.path().to_path_buf()];
    let script = "
- tool_calls: [{name: exec, arguments: {cmd: 'echo forbidden'}}]
- tool_calls: [{name: exec, arguments: {cmd: 'echo SECRET'}}]
- tool_calls: [{name: task_complete}]
";
    let r = run_script(&spec, "x", script).await;
    let reqs = r.provider.requests();
    assert_eq!(result_text(&reqs[1]), "Blocked by a hook: not allowed here");
    assert_eq!(result_text(&reqs[2]), "[redacted]");
    let c = r.counters();
    assert_eq!(c["hook_blocks"], 1);
    assert_eq!(c["hook_modifies"], 1);

    // hooks: false in gitagent.yaml turns them off.
    spec.config.hooks = false;
    let r = run_script(&spec, "x", script).await;
    assert!(result_text(&r.provider.requests()[1]).starts_with("forbidden\n"));
}

#[tokio::test]
async fn skills_are_indexed_and_loaded_on_demand() {
    let cfg = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(cfg.path().join(".gitagent/skills/deploy")).unwrap();
    std::fs::write(
        cfg.path().join(".gitagent/skills/deploy/SKILL.md"),
        "---\nname: deploy\ndescription: How to deploy the service.\n---\nRun make deploy, then check /health.\n",
    )
    .unwrap();
    let spec = load(&LoadOptions {
        model: Some("scripted:test".into()),
        project_dir: Some(cfg.path().to_path_buf()),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(spec.skills.len(), 1);
    assert!(spec
        .system_prompt
        .ends_with("- deploy: How to deploy the service.\n"));
    let script = "
- tool_calls: [{name: skill, arguments: {name: deploy}}]
- tool_calls: [{name: task_complete}]
";
    let r = run_script(&spec, "x", script).await;
    let reqs = r.provider.requests();
    assert_eq!(reqs[0].tools.last().unwrap().name, "skill");
    let text = result_text(&reqs[1]);
    assert!(
        text.starts_with("Skill deploy (files next to it are in "),
        "{text}"
    );
    assert!(text.ends_with("Run make deploy, then check /health."));
}

#[tokio::test]
async fn subagents_run_with_a_fresh_context_and_return_only_a_summary() {
    let cfg = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(cfg.path().join(".gitagent/agents")).unwrap();
    std::fs::write(
        cfg.path().join(".gitagent/agents/checker.md"),
        "---\nname: checker\ndescription: Runs checks.\ntools: exec\nmax_turns: 5\n---\nOnly run commands; do not edit.\n",
    )
    .unwrap();
    let mut spec = load(&LoadOptions {
        model: Some("scripted:test".into()),
        project_dir: Some(cfg.path().to_path_buf()),
        ..Default::default()
    })
    .unwrap();
    assert!(spec.agents.is_empty(), "sub-agents are off by default");
    spec.profile.tools.subagents = true;
    spec.agents = vec![gitagent_loader::AgentDef {
        name: "checker".into(),
        description: "Runs checks.".into(),
        tools: Some(vec!["exec".into()]),
        max_turns: 5,
        disallowed_tools: Vec::new(),
        model: None,
        effort: None,
        instructions: "Only run commands; do not edit.".into(),
        root: None,
        triggers: Vec::new(),
        delegation: None,
    }];
    // Parent turn 1 starts the sub-agent; its two turns follow; then the
    // parent finishes.
    let script = r#"
- tool_calls: [{name: subagent, arguments: {description: count files, prompt: "Count the files here.", agent: checker}}]
- tool_calls: [{name: exec, arguments: {cmd: "echo child-was-here > marker.txt; ls | wc -l"}}]
- tool_calls: [{name: task_complete, arguments: {summary: "There is 1 file (marker.txt)."}}]
- tool_calls: [{name: task_complete, arguments: {summary: done}}]
"#;
    let r = run_script_for(&spec, "Ask a helper to count files.", script, 600).await;
    assert_eq!(r.report.outcome, Outcome::Completed);
    let reqs = r.provider.requests();
    // The child: fresh history, its own role, only the allowed tools.
    let child_first = &reqs[1];
    assert_eq!(child_first.messages.len(), 1);
    assert!(child_first.system.contains("# Sub-agent"));
    assert!(child_first
        .system
        .contains("Only run commands; do not edit."));
    let names: Vec<&str> = child_first.tools.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["exec", "task_complete"]);
    // The parent sees only the summary.
    let back = result_text(&reqs[3]);
    assert!(
        back.starts_with("Sub-agent `checker` (count files) ended: completed after 2 turns."),
        "{back}"
    );
    assert!(back.ends_with("There is 1 file (marker.txt)."));
    assert!(!back.contains("child-was-here"));
    // Records: the child's own trajectory, linked from the parent's.
    let child_traj = r.dir.path().join("out/subagents/1/trajectory.json");
    assert!(child_traj.exists());
    let t = r.trajectory();
    assert_eq!(
        t["steps"][2]["observation"]["results"][0]["subagent_trajectory_ref"][0]["trajectory_path"],
        child_traj.display().to_string()
    );
    gitagent_record::atif::validate(&serde_json::from_value(t).unwrap()).unwrap();
    let c = r.counters();
    assert_eq!(c["subagent_calls"], 1);
    assert_eq!(c["subagent_turns"], 2);
    // The parent cannot start nested sub-agents through the child.
    assert!(reqs[0].tools.iter().any(|t| t.name == "subagent"));
}

/// Approves calls whose arguments mention "approved", records every
/// request, and can trip a kill switch when asked.
struct TestApprover {
    seen: std::sync::Mutex<Vec<ApprovalRequest>>,
    kill: Option<KillSwitch>,
}

#[async_trait::async_trait]
impl Approver for TestApprover {
    async fn approve(&self, request: &ApprovalRequest) -> Approval {
        self.seen.lock().unwrap().push(request.clone());
        if let Some(k) = &self.kill {
            k.trip();
            return Approval::denied("stopping");
        }
        if Value::Object(request.arguments.clone())
            .to_string()
            .contains("approved")
        {
            Approval {
                approved: true,
                reason: None,
            }
        } else {
            Approval::denied("not this one")
        }
    }
}

async fn run_supervised(
    spec: &AgentSpec,
    script: &str,
    approver: Option<std::sync::Arc<TestApprover>>,
    kill: Option<KillSwitch>,
) -> Run {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("work")).unwrap();
    let provider = ScriptedProvider::from_yaml(script).unwrap();
    let report = run(
        spec,
        &provider,
        RunOptions {
            instruction: "x".into(),
            work_dir: dir.path().join("work"),
            out_dir: dir.path().join("out"),
            timeout: Some(Duration::from_secs(60)),
            session_id: None,
            events: gitagent_record::Events::disabled(),
            resume: None,
            sandbox: None,
            tool_allowlist: None,
            tool_denylist: Vec::new(),
            hook_callbacks: None,
            mcp: None,
            approver: approver.map(|a| a as std::sync::Arc<dyn Approver>),
            kill,
            steer: None,
            external_tools: None,
        },
    )
    .await
    .unwrap()
    .report;
    Run {
        dir,
        report,
        provider,
    }
}

fn test_approver(kill: Option<KillSwitch>) -> std::sync::Arc<TestApprover> {
    std::sync::Arc::new(TestApprover {
        seen: Default::default(),
        kill,
    })
}

#[tokio::test]
async fn always_supervision_asks_before_calls_that_change_something() {
    let mut spec = spec();
    spec.supervision.hitl = gitagent_loader::compliance::Hitl::Always;
    let script = "- tool_calls: [{name: exec, arguments: {cmd: \"echo approved > a.txt\"}}]\n\
                  - tool_calls: [{name: exec, arguments: {cmd: \"rm -rf a.txt\"}}]\n\
                  - tool_calls: [{name: read_file, arguments: {path: a.txt}}]\n\
                  - tool_calls: [{name: task_complete, arguments: {summary: done}}]\n";
    let approver = test_approver(None);
    let r = run_supervised(&spec, script, Some(approver.clone()), None).await;
    assert_eq!(r.report.outcome, Outcome::Completed);
    let seen = approver.seen.lock().unwrap().clone();
    let asked: Vec<&str> = seen.iter().map(|q| q.tool_name.as_str()).collect();
    assert_eq!(asked, vec!["exec", "exec"]);
    assert!(seen[0].reason.contains("human_in_the_loop: always"));
    assert!(r.dir.path().join("work/a.txt").exists());
    let c = r.counters();
    assert_eq!(
        (
            c["approvals_granted"].as_u64(),
            c["approvals_denied"].as_u64()
        ),
        (Some(1), Some(1))
    );
    let t = r.trajectory().to_string();
    assert!(t.contains("was denied: not this one"), "{t}");

    // Nobody to ask: denied.
    let r = run_supervised(&spec, script, None, None).await;
    assert!(r
        .trajectory()
        .to_string()
        .contains("denied: no one is available to approve it"));
    assert!(!r.dir.path().join("work/a.txt").exists());
}

#[tokio::test]
async fn conditional_supervision_asks_when_a_trigger_fires() {
    use gitagent_loader::compliance::{Hitl, Trigger};
    let mut spec = spec();
    spec.supervision.hitl = Hitl::Conditional;
    spec.supervision.triggers = vec![Trigger::ErrorDetected];
    let script = "- tool_calls: [{name: write_file, arguments: {path: a.txt, content: x}}]\n\
                  - tool_calls: [{name: read_file, arguments: {path: missing.txt}}]\n\
                  - tool_calls: [{name: write_file, arguments: {path: b.txt, content: approved}}]\n\
                  - tool_calls: [{name: write_file, arguments: {path: c.txt, content: x}}]\n\
                  - tool_calls: [{name: task_complete, arguments: {summary: done}}]\n";
    let approver = test_approver(None);
    let r = run_supervised(&spec, script, Some(approver.clone()), None).await;
    assert_eq!(r.report.outcome, Outcome::Completed);
    let seen = approver.seen.lock().unwrap().clone();
    // Only the write right after the failed read; the approval clears it.
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].arguments["path"], "b.txt");
    assert!(seen[0].reason.contains("error_detected"));
    for f in ["a.txt", "b.txt", "c.txt"] {
        assert!(r.dir.path().join("work").join(f).exists(), "{f}");
    }

    spec.supervision.triggers = vec![Trigger::ActionType(vec!["read_file".into()])];
    let approver = test_approver(None);
    run_supervised(&spec, script, Some(approver.clone()), None).await;
    let seen = approver.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].tool_name, "read_file");
}

#[tokio::test]
async fn the_kill_switch_stops_the_run_between_steps() {
    let mut spec = spec();
    spec.supervision.hitl = gitagent_loader::compliance::Hitl::Always;
    // Tripped while the first of two calls waits for approval: the second
    // is not run and no further model call is made.
    let kill = KillSwitch::default();
    let script = "- tool_calls: [{name: exec, arguments: {cmd: \"echo 1 > one\"}}, {name: exec, arguments: {cmd: \"echo 2 > two\"}}]\n\
                  - tool_calls: [{name: task_complete, arguments: {summary: done}}]\n";
    let r = run_supervised(
        &spec,
        script,
        Some(test_approver(Some(kill.clone()))),
        Some(kill.clone()),
    )
    .await;
    assert_eq!(
        r.report.outcome,
        Outcome::Killed("the kill switch was used".into())
    );
    assert_eq!(r.provider.requests().len(), 1);
    assert!(!r.dir.path().join("work/two").exists());
    assert!(r
        .trajectory()
        .to_string()
        .contains("Not run: the agent was halted"));

    // Already tripped: no model call at all.
    let r = run_supervised(&spec, script, None, Some(kill)).await;
    assert!(matches!(r.report.outcome, Outcome::Killed(_)));
    assert!(r.provider.requests().is_empty());
}

#[tokio::test]
async fn reviewed_responses_wait_for_approval() {
    let mut spec = spec();
    spec.supervision.review_responses = true;
    let script = "- tool_calls: [{name: task_complete, arguments: {summary: first}}]\n\
                  - tool_calls: [{name: task_complete, arguments: {summary: approved answer}}]\n";
    let approver = test_approver(None);
    let r = run_supervised(&spec, script, Some(approver.clone()), None).await;
    assert_eq!(r.report.outcome, Outcome::Completed);
    assert_eq!(r.report.summary.as_deref(), Some("approved answer"));
    assert_eq!(approver.seen.lock().unwrap().len(), 2);
    assert!(r
        .trajectory()
        .to_string()
        .contains("Your final response was not approved: not this one"));
}

#[tokio::test]
async fn declared_tools_marked_for_confirmation_always_ask() {
    let tools_dir = tempfile::tempdir().unwrap();
    let script_path = tools_dir.path().join("t.sh");
    std::fs::write(&script_path, "cat >/dev/null; echo ran\n").unwrap();
    let mut spec = spec();
    spec.declared_tools.push(gitagent_loader::DeclaredTool {
        name: "pay".into(),
        description: "Pays.".into(),
        input_schema: json!({"type": "object", "properties": {"note": {"type": "string"}}}),
        runner: gitagent_loader::declared::Runner::Script {
            command: vec!["sh".into(), script_path.display().to_string()],
            dir: tools_dir.path().to_path_buf(),
        },
        timeout: Duration::from_secs(10),
        requires_confirmation: true,
        read_only: true,
    });
    let script = "- tool_calls: [{name: pay, arguments: {note: nope}}]\n\
                  - tool_calls: [{name: pay, arguments: {note: approved}}]\n\
                  - tool_calls: [{name: task_complete, arguments: {summary: done}}]\n";
    let approver = test_approver(None);
    let r = run_supervised(&spec, script, Some(approver.clone()), None).await;
    let seen = approver.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].reason, "pay asks for confirmation before each call");
    let t = r.trajectory().to_string();
    assert_eq!(t.matches("\"content\":\"ran\"").count(), 1, "{t}");
}

#[tokio::test]
async fn in_conversation_mode_a_plain_reply_ends_the_turn() {
    let chat = load(&LoadOptions {
        model: Some("scripted:test".into()),
        conversation: true,
        ..Default::default()
    })
    .unwrap();
    assert!(chat.system_prompt.contains("# Ending your turn"));
    assert!(!chat.system_prompt.contains("task_complete"));
    let r = run_script(
        &chat,
        "What is in hello.txt?",
        "- tool_calls: [{name: exec, arguments: {cmd: \"echo hi > hello.txt && cat hello.txt\"}}]\n\
         - text: \"It says hi.\"\n",
    )
    .await;
    assert_eq!(r.report.outcome, Outcome::Completed);
    assert_eq!(r.report.summary.as_deref(), Some("It says hi."));
    assert_eq!(r.counters()["conversation_replies"], 1);
    let names: Vec<String> = r.provider.requests()[0]
        .tools
        .iter()
        .map(|t| t.name.clone())
        .collect();
    assert!(!names.contains(&"task_complete".to_string()), "{names:?}");

    // Task mode keeps asking for a tool call.
    let r = run_script(
        &spec(),
        "x",
        "- text: \"Done.\"\n- text: \"Done.\"\n- text: \"Done.\"\n",
    )
    .await;
    assert_eq!(r.report.outcome, Outcome::NoToolCalls);
}

/// A model that takes a long time to answer.
struct SlowProvider;

#[async_trait::async_trait]
impl gitagent_provider::Provider for SlowProvider {
    async fn complete(
        &self,
        _req: &gitagent_provider::ChatRequest,
    ) -> std::result::Result<gitagent_provider::ChatResponse, gitagent_provider::ProviderError>
    {
        tokio::time::sleep(Duration::from_secs(30)).await;
        Err(gitagent_provider::ProviderError::Fatal("too slow".into()))
    }

    fn family(&self) -> gitagent_loader::Family {
        gitagent_loader::Family::Compat
    }
}

#[tokio::test]
async fn the_kill_switch_cancels_a_model_call_in_progress() {
    let spec = spec();
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("work")).unwrap();
    let kill = KillSwitch::default();
    let k = kill.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        k.stop("aborted by the test");
    });
    let started = Instant::now();
    let finished = run(
        &spec,
        &SlowProvider,
        RunOptions {
            instruction: "x".into(),
            work_dir: dir.path().join("work"),
            out_dir: dir.path().join("out"),
            timeout: Some(Duration::from_secs(60)),
            session_id: None,
            events: gitagent_record::Events::disabled(),
            resume: None,
            sandbox: None,
            tool_allowlist: None,
            tool_denylist: Vec::new(),
            hook_callbacks: None,
            mcp: None,
            approver: None,
            kill: Some(kill),
            steer: None,
            external_tools: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        finished.report.outcome,
        Outcome::Killed("aborted by the test".into())
    );
    assert!(started.elapsed() < Duration::from_secs(5));
    // The conversation and shell survive for the next turn.
    assert_eq!(finished.messages.len(), 1);
}

#[tokio::test]
async fn a_sub_agent_directory_runs_as_its_own_agent() {
    let dir = tempfile::tempdir().unwrap();
    let lead = dir.path().join("lead");
    let write = |p: PathBuf, t: &str| {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, t).unwrap();
    };
    write(
        lead.join("agent.yaml"),
        "name: lead\nversion: 1.0.0\ndescription: Lead.\nagents:\n  helper: {description: Checks things., delegation: {triggers: [check_requested]}}\n",
    );
    write(lead.join("SOUL.md"), "Lead soul.\n");
    write(
        lead.join("agents/helper/agent.yaml"),
        "name: helper\nversion: 1.0.0\ndescription: Helper.\nruntime: {max_turns: 5}\n",
    );
    write(lead.join("agents/helper/SOUL.md"), "I am Helper.\n");
    write(lead.join("agents/helper/RULES.md"), "- Helper rule.\n");
    let spec = load(&LoadOptions {
        project_dir: Some(lead.clone()),
        model: Some("scripted:test".into()),
        ..Default::default()
    })
    .unwrap();
    assert!(spec.profile.tools.subagents);
    let names: Vec<&str> = spec.agents.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(names, vec!["general", "helper"]);
    assert!(spec
        .system_prompt
        .contains("- helper: Checks things. Use it when: check_requested.\n"));

    let r = run_script_for(
        &spec,
        "check it",
        "- tool_calls: [{name: subagent, arguments: {description: check, prompt: \"check the thing\", agent: helper}}]\n\
         - tool_calls: [{name: task_complete, arguments: {summary: \"Helper checked: fine\"}}]\n\
         - tool_calls: [{name: task_complete, arguments: {summary: done}}]\n",
        600,
    )
    .await;
    assert_eq!(r.report.outcome, Outcome::Completed);
    let parent = r.trajectory().to_string();
    assert!(parent.contains("Helper checked: fine"));
    assert!(
        !parent.contains("could not") && !parent.contains("Not enough time"),
        "{parent}"
    );
    let child: Value = serde_json::from_slice(
        &std::fs::read(r.dir.path().join("out/subagents/1/trajectory.json")).unwrap(),
    )
    .unwrap();
    let prompt = child["steps"][0]["message"].as_str().unwrap();
    assert!(
        prompt.starts_with("# helper v1.0.0\nHelper.\n\nI am Helper.\n\n- Helper rule."),
        "{prompt}"
    );
    assert!(!prompt.contains("Lead soul."));
    assert!(prompt.contains("# Sub-agent\n"));
    assert_eq!(child["extra"]["counters"]["turns"], 1);
}

#[tokio::test]
async fn teardown_runs_one_last_turn_on_the_sessions_history() {
    let dir = tempfile::tempdir().unwrap();
    let agent = dir.path().join("agent");
    std::fs::create_dir_all(agent.join("hooks")).unwrap();
    std::fs::write(
        agent.join("agent.yaml"),
        "name: a\nversion: 1.0.0\ndescription: A.\n",
    )
    .unwrap();
    std::fs::write(agent.join("SOUL.md"), "I am A.\n").unwrap();
    std::fs::write(agent.join("hooks/bootstrap.md"), "Check the clock.\n").unwrap();
    std::fs::write(
        agent.join("hooks/teardown.md"),
        "Write a note to notes.txt.\n",
    )
    .unwrap();
    let spec = load(&LoadOptions {
        project_dir: Some(agent.clone()),
        model: Some("scripted:test".into()),
        ..Default::default()
    })
    .unwrap();
    assert!(spec.system_prompt.ends_with("# Session start\n\nAt the start of a session, before the first request, do the following (once; the conversation shows whether it is done):\n\nCheck the clock.\n"));
    assert_eq!(spec.teardown.as_deref(), Some("Write a note to notes.txt."));

    let work = dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let provider = ScriptedProvider::from_yaml(
        "- tool_calls: [{name: task_complete, arguments: {summary: done}}]\n\
         - tool_calls: [{name: exec, arguments: {cmd: \"echo bye > notes.txt\"}}]\n\
         - text: \"Wrote the note.\"\n",
    )
    .unwrap();
    let opts = |out: &str, resume| RunOptions {
        instruction: "x".into(),
        work_dir: work.clone(),
        out_dir: dir.path().join(out),
        timeout: Some(Duration::from_secs(60)),
        session_id: None,
        events: gitagent_record::Events::disabled(),
        resume,
        sandbox: None,
        tool_allowlist: None,
        tool_denylist: Vec::new(),
        hook_callbacks: None,
        mcp: None,
        approver: None,
        kill: None,
        steer: None,
        external_tools: None,
    };
    let finished = run(&spec, &provider, opts("out", None)).await.unwrap();
    assert_eq!(finished.report.outcome, Outcome::Completed);
    let done = teardown(
        &spec,
        &provider,
        opts("teardown", Some(finished.into_resume())),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(done.report.outcome, Outcome::Completed);
    assert_eq!(done.report.summary.as_deref(), Some("Wrote the note."));
    assert_eq!(
        std::fs::read_to_string(work.join("notes.txt")).unwrap(),
        "bye\n"
    );
    let t: Value = serde_json::from_slice(
        &std::fs::read(dir.path().join("teardown/trajectory.json")).unwrap(),
    )
    .unwrap();
    let last_user = t["steps"]
        .as_array()
        .unwrap()
        .iter()
        .rfind(|s| s["source"] == "user")
        .unwrap();
    assert!(last_user["message"].as_str().unwrap().contains("# Session end\n\nThe session is ending. Do the following now, then reply with one line on what you did:\n\nWrite a note to notes.txt."));
    // Without a teardown there is nothing to run.
    let plain = load(&LoadOptions {
        model: Some("scripted:test".into()),
        ..Default::default()
    })
    .unwrap();
    let finished = run(&plain, &provider, opts("out2", None)).await.unwrap();
    assert!(
        teardown(&plain, &provider, opts("t2", Some(finished.into_resume())))
            .await
            .is_none()
    );
}

/// A workflow: a tool step run directly, a model step whose reply carries
/// the outputs, a step skipped by its condition, a step needing approval,
/// and a failing step under `on_step_failure: skip`.
#[tokio::test]
async fn workflows_run_their_steps_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let agent = dir.path().join("agent");
    let write = |p: PathBuf, t: &str| {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, t).unwrap();
    };
    write(
        agent.join("agent.yaml"),
        "name: a\nversion: 1.0.0\ndescription: A.\n",
    );
    write(agent.join("SOUL.md"), "I am A.\n");
    write(agent.join("tools/count.sh"), "input=$(cat); n=$(printf '%s' \"$input\" | tr -cd ',' | wc -c); printf '{\"count\": %s}' $((n + 1))\n");
    write(
        agent.join("tools/count.yaml"),
        "name: count\ndescription: Counts items.\ninput_schema: {type: object, properties: {items: {type: string}}}\nimplementation: {script: count.sh}\n",
    );
    write(agent.join("tools/fail.sh"), "echo nope >&2; exit 1\n");
    write(
        agent.join("tools/fail.yaml"),
        "name: fail\ndescription: Fails.\nimplementation: {script: fail.sh}\n",
    );
    write(
        agent.join("workflows/review.yaml"),
        "name: review\ndescription: Review items.\ninputs: [{name: items, type: string}, {name: mode, default: quick, required: false}]\noutputs: [{name: verdict, type: string}]\nsteps:\n  - id: count\n    action: Count the items.\n    tool: count\n    inputs: {items: \"${{ inputs.items }}\"}\n    outputs: [count]\n  - id: judge\n    action: Judge the items.\n    depends_on: [count]\n    inputs: {n: \"${{ steps.count.outputs.count }}\", mode: \"${{ inputs.mode }}\"}\n    outputs: [verdict, score]\n  - id: deep\n    action: Look deeper.\n    depends_on: [judge]\n    conditions: [\"${{ steps.judge.outputs.score > 9 }}\"]\n  - id: sign\n    action: Sign it off.\n    depends_on: [judge]\n    compliance: {requires_approval: true}\n    outputs: [signed]\n  - id: broken\n    action: Break.\n    tool: fail\n    depends_on: [sign]\nerror_handling: {on_step_failure: skip}\n",
    );
    let spec = load(&LoadOptions {
        project_dir: Some(agent.clone()),
        model: Some("scripted:test".into()),
        ..Default::default()
    })
    .unwrap();
    assert!(spec
        .system_prompt
        .contains("- review: Review items. Inputs: items, mode?.\n"));
    // The judge step's turn, then the sign step's turn.
    let provider = ScriptedProvider::from_yaml(
        "- text: \"Three items in quick mode look fine. {\\\"verdict\\\": \\\"ok\\\", \\\"score\\\": 7}\"\n\
         - text: \"{\\\"signed\\\": true}\"\n",
    )
    .unwrap();
    let approver = test_approver(None);
    let work = dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let inputs = json!({"items": "a,b,c"}).as_object().cloned().unwrap();
    let report = run_workflow(
        &spec,
        &provider,
        "review",
        &inputs,
        RunOptions {
            instruction: String::new(),
            work_dir: work.clone(),
            out_dir: dir.path().join("out"),
            timeout: Some(Duration::from_secs(600)),
            session_id: None,
            events: gitagent_record::Events::disabled(),
            resume: None,
            sandbox: None,
            tool_allowlist: None,
            tool_denylist: Vec::new(),
            hook_callbacks: None,
            mcp: None,
            approver: Some(approver.clone()),
            kill: None,
            steer: None,
            external_tools: None,
        },
    )
    .await
    .unwrap();
    let statuses: Vec<(&str, String)> = report
        .steps
        .iter()
        .map(|s| (s.id.as_str(), format!("{:?}", s.status)))
        .collect();
    assert_eq!(
        statuses,
        vec![
            ("count", "Done".to_string()),
            ("judge", "Done".to_string()),
            ("deep", "Skipped".to_string()),
            ("sign", "Failed(\"not approved: not this one\")".to_string()),
            (
                "broken",
                "Failed(\"Tool fail failed: exited with code 1: nope\")".to_string()
            ),
        ]
    );
    assert_eq!(report.steps[0].outputs["count"], 3);
    assert_eq!(report.steps[1].outputs["verdict"], "ok");
    assert_eq!(report.outputs["verdict"], "ok");
    assert!(report.stopped.is_none() && !report.succeeded());
    // The judge step saw the count and the default input.
    let judge: Value = serde_json::from_slice(
        &std::fs::read(
            dir.path()
                .join("out/steps/2-judge/attempt-1/trajectory.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let asked = judge["steps"][1]["message"].as_str().unwrap();
    assert!(
        asked.contains("# Workflow review, step judge\n\nJudge the items.")
            && asked.contains("\"n\": 3")
            && asked.contains("\"mode\": \"quick\""),
        "{asked}"
    );
    let seen = approver.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    assert!(seen[0].reason.contains("step sign requires approval"));
    assert!(report.text().contains("- deep: skipped"));

    // Abort stops at the first failure; unknown workflows and missing inputs are errors.
    let err = run_workflow(
        &spec,
        &provider,
        "nope",
        &Map::new(),
        RunOptions {
            instruction: String::new(),
            work_dir: work.clone(),
            out_dir: dir.path().join("out2"),
            timeout: None,
            session_id: None,
            events: gitagent_record::Events::disabled(),
            resume: None,
            sandbox: None,
            tool_allowlist: None,
            tool_denylist: Vec::new(),
            hook_callbacks: None,
            mcp: None,
            approver: None,
            kill: None,
            steer: None,
            external_tools: None,
        },
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(err.contains("no workflow named \"nope\""), "{err}");
}

/// The compliance runtime: the audit log records what `log_contents`
/// names, a handoff blocks a caller without the role and asks when it has
/// it, payments follow the spending rules and add up in the month's state,
/// and a sub-agent holding conflicting roles does not run.
#[tokio::test]
async fn audit_log_handoffs_and_spending_rules() {
    let dir = tempfile::tempdir().unwrap();
    let agent = dir.path().join("agent");
    let write = |p: PathBuf, t: &str| {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, t).unwrap();
    };
    write(
        agent.join("agent.yaml"),
        "name: teller\nversion: 1.0.0\ndescription: Pays.\nagents:\n  both: {description: Holds both roles.}\n  payer: {description: Pays.}\ncompliance:\n  risk_tier: high\n  recordkeeping: {audit_logging: true, log_contents: [tool_calls, model_version]}\n  segregation_of_duties:\n    roles: [{id: maker, description: m}, {id: approver, description: a}]\n    conflicts: [[maker, approver]]\n    assignments: {teller: [maker], both: [maker, approver], payer: [maker]}\n    handoffs:\n      - {action: transfer, required_roles: [maker, approver]}\n      - {action: audit-export, required_roles: [approver], approval_required: false}\n  financial_governance:\n    enabled: true\n    spending: {max_per_transaction_cents: 5000, max_monthly_cents: 8000, blocked_categories: [gambling], allowed_categories: [office, travel]}\n    approval: {require_above_cents: 2000, timeout_minutes: 1, auto_deny_on_timeout: true}\n",
    );
    write(agent.join("SOUL.md"), "I pay.\n");
    write(
        agent.join("agents/both/agent.yaml"),
        "name: both\nversion: 1.0.0\ndescription: Both.\n",
    );
    write(agent.join("agents/both/SOUL.md"), "I hold both.\n");
    write(
        agent.join("agents/payer/agent.yaml"),
        "name: payer\nversion: 1.0.0\ndescription: Payer.\n",
    );
    write(agent.join("agents/payer/SOUL.md"), "I pay too.\n");
    for name in ["transfer", "audit-export", "pay"] {
        write(
            agent.join(format!("tools/{name}.sh")),
            "cat >/dev/null; echo '{\"text\": \"done\"}'\n",
        );
        write(
            agent.join(format!("tools/{name}.yaml")),
            &format!("name: {name}\ndescription: {name}.\ninput_schema: {{type: object, properties: {{amount_cents: {{type: integer}}, category: {{type: string}}, note: {{type: string}}}}}}\nimplementation: {{script: {name}.sh}}\n"),
        );
    }
    let spec = load(&LoadOptions {
        project_dir: Some(agent.clone()),
        model: Some("scripted:test".into()),
        ..Default::default()
    })
    .unwrap();
    let audit = spec.audit.as_ref().unwrap();
    assert_eq!(audit.dir, agent.join(".gitagent/audit"));
    assert!(audit.logs("tool_calls") && !audit.logs("prompts_and_responses"));

    let script = "- tool_calls: [{name: transfer, arguments: {note: approved}}]\n\
                  - tool_calls: [{name: audit-export, arguments: {}}]\n\
                  - tool_calls: [{name: pay, arguments: {amount_cents: 300, category: gambling}}]\n\
                  - tool_calls: [{name: pay, arguments: {amount_cents: 300, category: food}}]\n\
                  - tool_calls: [{name: pay, arguments: {amount_cents: 9000, category: office}}]\n\
                  - tool_calls: [{name: pay, arguments: {amount_cents: 1500, category: office}}]\n\
                  - tool_calls: [{name: pay, arguments: {amount_cents: 4500, category: travel, note: approved}}]\n\
                  - tool_calls: [{name: pay, arguments: {amount_cents: 3000, category: travel, note: approved}}]\n\
                  - tool_calls: [{name: subagent, arguments: {description: x, prompt: go, agent: both}}]\n\
                  - tool_calls: [{name: task_complete, arguments: {summary: done}}]\n";
    let approver = test_approver(None);
    let r = run_supervised(&spec, script, Some(approver.clone()), None).await;
    assert_eq!(r.report.outcome, Outcome::Completed);
    let results: Vec<String> = r.trajectory()["steps"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| {
            s["observation"]["results"][0]["content"]
                .as_str()
                .map(str::to_string)
        })
        .collect();
    assert_eq!(results.len(), 10, "{results:#?}");
    // transfer: a handoff the teller takes part in (maker), approval asked.
    assert_eq!(results[0], "done");
    // audit-export: a handoff needing the approver role the teller lacks.
    assert!(
        results[1].starts_with("Not run: audit-export needs the roles approver"),
        "{}",
        results[1]
    );
    assert!(
        results[2].contains("the category gambling is blocked"),
        "{}",
        results[2]
    );
    assert!(
        results[3].contains("not among the allowed ones"),
        "{}",
        results[3]
    );
    assert!(
        results[4].contains("over the limit of 5000 per transaction"),
        "{}",
        results[4]
    );
    // 1500 is under the approval threshold and goes through.
    assert_eq!(results[5], "done");
    // 4500 needs approval (given) and goes through: 6000 this month.
    assert_eq!(results[6], "done");
    // 3000 more would pass the monthly cap of 8000.
    assert!(
        results[7].contains("would take this month's spending to 9000 of the 8000 allowed"),
        "{}",
        results[7]
    );
    assert!(
        results[8].contains("holds both maker and approver, which conflict"),
        "{}",
        results[8]
    );
    let seen: Vec<String> = approver
        .seen
        .lock()
        .unwrap()
        .iter()
        .map(|q| q.reason.clone())
        .collect();
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert!(seen[0].contains("transfer is a handoff through the roles maker → approver"));
    assert!(seen[1].contains("a payment of 4500 cents is over 2000"));
    let state: Value =
        serde_json::from_slice(&std::fs::read(agent.join(".gitagent/state.json")).unwrap())
            .unwrap();
    assert_eq!(
        state["spending"]
            .as_object()
            .unwrap()
            .values()
            .next()
            .unwrap(),
        6000
    );

    // The audit log: tool calls and model versions, no prompts or texts.
    let mut files: Vec<_> = std::fs::read_dir(agent.join(".gitagent/audit"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    files.sort();
    assert_eq!(files.len(), 1);
    let log = std::fs::read_to_string(&files[0]).unwrap();
    let records: Vec<Value> = log
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let events: Vec<&str> = records
        .iter()
        .map(|r| r["event"].as_str().unwrap())
        .collect();
    assert_eq!(events[0], "run_start");
    assert!(
        events.iter().filter(|e| **e == "tool_use").count() >= 9,
        "{events:?}"
    );
    assert!(
        events.contains(&"approval_request")
            && events.contains(&"approval")
            && events.contains(&"run_end")
    );
    assert!(records.iter().any(|r| r["event"] == "response"
        && r["model"] == "scripted:test"
        && r.get("text").is_none()));
    assert!(records
        .iter()
        .all(|r| r["session_id"] == "-" && r["timestamp"].is_string()));
    assert!(!events.contains(&"prompt"), "{events:?}");

    // A strict agent holding both roles does not load at all.
    write(
        agent.join("agent.yaml"),
        "name: teller\nversion: 1.0.0\ndescription: Pays.\ncompliance:\n  segregation_of_duties:\n    roles: [{id: maker, description: m}, {id: approver, description: a}]\n    conflicts: [[maker, approver]]\n    assignments: {teller: [maker, approver]}\n",
    );
    let err = load(&LoadOptions {
        project_dir: Some(agent.clone()),
        model: Some("scripted:test".into()),
        ..Default::default()
    })
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("teller holds both maker and approver, which conflict (enforcement: strict)"),
        "{err}"
    );
}

#[tokio::test]
async fn a_workflow_step_naming_a_missing_tool_fails_plainly() {
    let dir = tempfile::tempdir().unwrap();
    let agent = dir.path().join("agent");
    std::fs::create_dir_all(agent.join("workflows")).unwrap();
    std::fs::write(
        agent.join("agent.yaml"),
        "name: a\nversion: 1.0.0\ndescription: A.\n",
    )
    .unwrap();
    std::fs::write(agent.join("SOUL.md"), "I am A.\n").unwrap();
    std::fs::write(
        agent.join("workflows/w.yaml"),
        "name: w\nsteps:\n  - {id: s, action: Go., tool: nonexistent}\n",
    )
    .unwrap();
    let spec = load(&LoadOptions {
        project_dir: Some(agent.clone()),
        model: Some("scripted:test".into()),
        ..Default::default()
    })
    .unwrap();
    let provider = ScriptedProvider::from_yaml("- text: should not be asked\n").unwrap();
    let report = run_workflow(
        &spec,
        &provider,
        "w",
        &Map::new(),
        RunOptions {
            instruction: String::new(),
            work_dir: agent.clone(),
            out_dir: dir.path().join("out"),
            timeout: None,
            session_id: None,
            events: gitagent_record::Events::disabled(),
            resume: None,
            sandbox: None,
            tool_allowlist: None,
            tool_denylist: Vec::new(),
            hook_callbacks: None,
            mcp: None,
            approver: None,
            kill: None,
            steer: None,
            external_tools: None,
        },
    )
    .await
    .unwrap();
    assert!(
        matches!(&report.steps[0].status, crate::workflow::StepStatus::Failed(why) if why.contains("\"nonexistent\" is not available")),
        "{:?}",
        report.steps[0].status
    );
    assert!(provider.requests().is_empty());
}

/// `plan`: each phase's tasks run at the same time as sub-agents, a later
/// phase sees the earlier results, and a reconciling sub-agent's answer
/// comes back as the tool's result.
#[tokio::test]
async fn a_plan_runs_phases_in_parallel_and_reconciles() {
    let mut spec = spec();
    spec.profile.tools.subagents = true;
    spec.profile.tools.plan = true;
    spec.profile.tools.max_concurrent_subagents = 2;
    spec.agents = vec![agent("worker", 5)];
    // The children draw from one script in the order they finish: three
    // phase-1 tasks, one phase-2 task, then the reconciler.
    let script = r#"
- tool_calls:
    - name: plan
      arguments:
        goal: Find every TODO.
        phases:
          - name: read
            tasks:
              - {description: read a.rs, prompt: "list the TODOs in a.rs", agent: worker}
              - {description: read b.rs, prompt: "list the TODOs in b.rs", agent: worker}
              - {description: read c.rs, prompt: "list the TODOs in c.rs", agent: worker}
          - name: check
            tasks:
              - {description: check them, prompt: "confirm each TODO still applies", agent: worker}
        reconcile: Combine the results into one list, no duplicates.
        agent: worker
- tool_calls: [{name: task_complete, arguments: {summary: "a.rs: 2 TODOs"}}]
- tool_calls: [{name: task_complete, arguments: {summary: "b.rs: 1 TODO"}}]
- tool_calls: [{name: task_complete, arguments: {summary: "c.rs: none"}}]
- tool_calls: [{name: task_complete, arguments: {summary: "all 3 still apply"}}]
- tool_calls: [{name: task_complete, arguments: {summary: "COMBINED: 3 TODOs in a.rs and b.rs"}}]
- tool_calls: [{name: task_complete, arguments: {summary: "parent done"}}]
"#;
    let r = run_script_for(&spec, "x", script, 600).await;
    assert_eq!(r.report.outcome, Outcome::Completed);
    let c = r.counters();
    assert_eq!(c["plan_runs"], 1);
    assert_eq!(c["plan_tasks"], 4);
    assert_eq!(c["plan_failed_tasks"], 0);
    assert_eq!(c["subagent_turns"], 5);
    let out = first_result(&r.trajectory());
    assert!(out.starts_with("4 tasks in 2 phases ran."), "{out}");
    assert!(out.contains("COMBINED: 3 TODOs"), "{out}");
    // The phase-2 task and the reconciler were given the earlier results.
    let texts = trajectories_under(&r.dir.path().join("out/plans"));
    assert_eq!(texts.len(), 5, "one trajectory per task and the reconciler");
    assert!(
        texts
            .iter()
            .any(|t| t.contains("Results of the earlier phases") && t.contains("read a.rs")),
        "the phase-2 task saw phase 1"
    );
    assert!(
        texts
            .iter()
            .any(|t| t.contains("Goal: Find every TODO.") && t.contains("# All results")),
        "the reconciler saw everything"
    );
}

#[tokio::test]
async fn a_bad_plan_is_refused_without_running_anything() {
    let mut spec = spec();
    spec.profile.tools.subagents = true;
    spec.profile.tools.plan = true;
    spec.agents = vec![agent("worker", 5)];
    let script = r#"
- tool_calls: [{name: plan, arguments: {phases: [{tasks: []}], reconcile: x}}]
- tool_calls: [{name: task_complete, arguments: {summary: done}}]
"#;
    let r = run_script_for(&spec, "x", script, 600).await;
    let out = first_result(&r.trajectory());
    assert!(out.contains("phase 1 has no tasks"), "{out}");
    assert_eq!(r.counters()["plan_tasks"], 0);
}

/// The content of the first tool result in a trajectory.
fn first_result(t: &serde_json::Value) -> String {
    t["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|s| s["observation"]["results"][0]["content"].as_str())
        .unwrap()
        .to_string()
}

/// Every trajectory.json under `dir`, as text.
fn trajectories_under(dir: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.file_name().is_some_and(|n| n == "trajectory.json") {
                out.push(std::fs::read_to_string(&p).unwrap());
            }
        }
    }
    out
}
