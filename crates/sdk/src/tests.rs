use super::*;

fn scripted(dir: &Path, name: &str, yaml: &str) -> String {
    let path = dir.join(name);
    std::fs::write(&path, yaml).unwrap();
    format!("scripted:{}", path.display())
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
async fn query_streams_sdk_shaped_messages() {
    let dir = tempfile::tempdir().unwrap();
    let model = scripted(dir.path(), "s.yaml", ECHO_THEN_COMPLETE);
    let mut q = query(
        "Print hi.",
        AgentOptions {
            model: Some(model),
            cwd: Some(dir.path().to_path_buf()),
            out_dir: Some(dir.path().join("out")),
            prices: Some(Prices {
                input: 4.0,
                output: 20.0,
                cache_read: 0.2,
                cache_write: 5.0,
            }),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let messages = q.collect().await;
    let report = q.finish().await.unwrap();
    assert_eq!(report.outcome, Outcome::Completed);

    let types: Vec<&str> = messages
        .iter()
        .map(|m| match m {
            Message::System { subtype, .. } => {
                if subtype == "init" {
                    "init"
                } else {
                    "system"
                }
            }
            Message::Assistant { .. } => "assistant",
            Message::User { .. } => "user",
            Message::Result { .. } => "result",
        })
        .collect();
    assert_eq!(
        types,
        vec!["init", "assistant", "user", "assistant", "user", "result"]
    );

    // The init message names the tools with `.claude/` names.
    let Message::System {
        tools,
        cwd,
        permission_mode,
        ..
    } = &messages[0]
    else {
        panic!()
    };
    assert!(tools.contains(&"Bash".to_string()) && tools.contains(&"task_complete".to_string()));
    assert_eq!(cwd.as_deref(), Some(dir.path().to_string_lossy().as_ref()));
    assert_eq!(permission_mode.as_deref(), Some("bypassPermissions"));

    // Turn 1: text plus a Bash tool_use with `.claude/` argument names, then its result.
    let Message::Assistant {
        message,
        parent_tool_use_id,
        ..
    } = &messages[1]
    else {
        panic!()
    };
    assert_eq!(parent_tool_use_id, &None);
    assert_eq!(message.role, "assistant");
    assert_eq!(message.stop_reason.as_deref(), Some("tool_use"));
    assert_eq!(
        message.content[0],
        ContentBlock::Text {
            text: "Checking the shell.".into()
        }
    );
    let ContentBlock::ToolUse { id, name, input } = &message.content[1] else {
        panic!()
    };
    assert_eq!(name, "Bash");
    assert_eq!(input, &json!({"command": "echo hi"}));
    let Message::User { message: user, .. } = &messages[2] else {
        panic!()
    };
    let ContentBlock::ToolResult {
        tool_use_id,
        content,
        is_error,
    } = &user.content[0]
    else {
        panic!()
    };
    assert_eq!((tool_use_id, *is_error), (id, false));
    assert!(content.contains("hi"), "{content}");

    // The result carries turns, usage, cost and the summary.
    let Message::Result {
        subtype,
        is_error,
        num_turns,
        result,
        total_cost_usd,
        usage,
        model_usage,
        stop_reason,
        ..
    } = messages.last().unwrap()
    else {
        panic!()
    };
    assert_eq!(
        (subtype.as_str(), *is_error, *num_turns),
        ("success", false, 2)
    );
    assert_eq!(result.as_deref(), Some("printed hi"));
    assert_eq!(stop_reason.as_deref(), Some("end_turn"));
    assert!(usage.input_tokens > 0 && *total_cost_usd > 0.0);
    assert_eq!(model_usage.len(), 1);

    // The JSON uses the stream's field names.
    let text = serde_json::to_string(messages.last().unwrap()).unwrap();
    assert!(
        text.contains("\"type\":\"result\"")
            && text.contains("\"modelUsage\"")
            && text.contains("\"num_turns\"")
    );
    let round: Message = serde_json::from_str(&text).unwrap();
    assert_eq!(&round, messages.last().unwrap());
}

#[tokio::test]
async fn hook_callbacks_and_tool_lists_apply() {
    let dir = tempfile::tempdir().unwrap();
    let script = "
- tool_calls:
    - name: exec
      arguments: {cmd: rm -rf /tmp/x}
- tool_calls:
    - name: exec
      arguments: {cmd: echo ok}
- tool_calls:
    - name: task_complete
      arguments: {summary: done}
";
    let model = scripted(dir.path(), "s.yaml", script);
    let seen = Arc::new(Mutex::new(Vec::<HookInput>::new()));
    let seen2 = seen.clone();
    let pre = HookMatcher::new(
        HookEvent::PreToolUse,
        Some("Bash"),
        move |input: HookInput| {
            seen2.lock().unwrap().push(input.clone());
            async move {
                let cmd = input.tool_input["command"].as_str().unwrap_or("");
                if cmd.starts_with("rm -rf") {
                    HookOutput::deny("no rm -rf")
                } else if cmd == "echo ok" {
                    let mut m = Map::new();
                    m.insert("command".into(), json!("echo changed"));
                    HookOutput::updated_input(m)
                } else {
                    HookOutput::allow()
                }
            }
        },
    );
    let post = HookMatcher::new(HookEvent::PostToolUse, None, |_input: HookInput| async {
        HookOutput::updated_tool_output("[output hidden by a hook]")
    });
    let mut q = query(
        "x",
        AgentOptions {
            model: Some(model),
            cwd: Some(dir.path().to_path_buf()),
            out_dir: Some(dir.path().join("out")),
            allowed_tools: vec!["Bash".into(), "Read".into()],
            disallowed_tools: vec!["Read".into()],
            hooks: vec![pre, post],
            max_turns: Some(5),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let messages = q.collect().await;
    q.finish().await.unwrap();

    let Message::System { tools, .. } = &messages[0] else {
        panic!()
    };
    assert_eq!(
        tools,
        &vec!["Bash".to_string(), "task_complete".to_string()]
    );

    // The hook saw `.claude/` tool names and argument names.
    let inputs = seen.lock().unwrap();
    assert_eq!(inputs[0].tool_name, "Bash");
    assert_eq!(inputs[0].hook_event_name, "PreToolUse");
    assert_eq!(inputs[0].tool_input["command"], "rm -rf /tmp/x");

    // Turn 1 was denied: the result names the denial; turn 2 ran the changed
    // command and its output was replaced by the post hook.
    let results: Vec<&ContentBlock> = messages
        .iter()
        .filter_map(|m| match m {
            Message::User { message, .. } => Some(&message.content[0]),
            _ => None,
        })
        .collect();
    let ContentBlock::ToolResult {
        content, is_error, ..
    } = results[0]
    else {
        panic!()
    };
    assert!(*is_error && content.contains("no rm -rf"), "{content}");
    let ContentBlock::ToolResult { content, .. } = results[1] else {
        panic!()
    };
    assert_eq!(content, "[output hidden by a hook]");
    let Message::Result {
        permission_denials, ..
    } = messages.last().unwrap()
    else {
        panic!()
    };
    assert_eq!(permission_denials.len(), 1);
    assert_eq!(permission_denials[0].tool_name, "Bash");
    assert_eq!(permission_denials[0].reason.as_deref(), Some("no rm -rf"));
}

#[tokio::test]
async fn agents_option_defines_subagents_and_the_system_prompt_can_be_set() {
    let dir = tempfile::tempdir().unwrap();
    let model = scripted(
        dir.path(),
        "s.yaml",
        "- tool_calls: [{name: task_complete, arguments: {summary: x}}]\n",
    );
    let mut agents = BTreeMap::new();
    agents.insert(
        "code-reviewer".to_string(),
        AgentDefinition {
            description: "Reviews code.".into(),
            prompt: "You review.".into(),
            tools: Some(vec!["Read".into(), "Grep".into()]),
            max_turns: Some(4),
            ..Default::default()
        },
    );
    let p = prepare(&AgentOptions {
        model: Some(model.clone()),
        cwd: Some(dir.path().to_path_buf()),
        system_prompt: Some("Be brief.".into()),
        append_system_prompt: Some("Always verify.".into()),
        agents,
        ..Default::default()
    })
    .unwrap();
    assert_eq!(p.spec.system_prompt, "Be brief.\n\nAlways verify.\n");
    assert!(p.spec.profile.tools.subagents);
    let names: Vec<&str> = p.spec.agents.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(names, vec!["code-reviewer", "general"]);
    let r = &p.spec.agents[0];
    assert_eq!(
        r.tools.as_deref(),
        Some(&["read_file".to_string(), "image_read".to_string()][..])
    );
    assert_eq!((r.max_turns, r.instructions.as_str()), (4, "You review."));

    // A bare model id means Anthropic.
    let p2 = prepare(&AgentOptions {
        model: Some("claude-opus-5-5".into()),
        cwd: Some(dir.path().to_path_buf()),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(p2.spec.model.to_string(), "anthropic:claude-opus-5-5");

    let mut q = query(
        "x",
        AgentOptions {
            model: Some(model),
            cwd: Some(dir.path().to_path_buf()),
            out_dir: Some(dir.path().join("out")),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let messages = q.collect().await;
    assert!(messages.last().unwrap().is_result());
}

#[test]
fn converter_handles_failed_runs_and_compaction() {
    let mut c = Converter::new("s", "m", 1000, None);
    let boundary = c.feed(&json!({"event": "guard", "kind": "compaction", "pre_tokens": 900}));
    let Message::System {
        subtype,
        compact_metadata,
        ..
    } = &boundary[0]
    else {
        panic!()
    };
    assert_eq!(subtype, "compact_boundary");
    assert_eq!(compact_metadata.as_ref().unwrap().pre_tokens, 900);
    let result =
        c.feed(&json!({"event": "run_end", "outcome": "max_turns", "turns": 7, "summary": null}));
    let Message::Result {
        subtype,
        is_error,
        num_turns,
        errors,
        result,
        ..
    } = &result[0]
    else {
        panic!()
    };
    assert_eq!(
        (subtype.as_str(), *is_error, *num_turns),
        ("error_max_turns", true, 7)
    );
    assert_eq!(errors, &vec!["max_turns".to_string()]);
    assert_eq!(result, &None);
    assert_eq!(outcome_of("error_max_turns"), Outcome::MaxTurns);
}
