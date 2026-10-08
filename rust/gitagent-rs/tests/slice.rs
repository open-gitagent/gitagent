//! Integration tests for the engine + SDK.

use futures_util::StreamExt;
use ira::pi::compact::Compactor;
use ira::pi::gate::{GateDecision, ToolGate};
use ira::pi::message::{AgentMessage, AssistantMessage, ContentBlock, StopReason, ToolResultMessage, Usage};
use ira::pi::provider::{is_transient_error, model_context_window, model_cost_per_mtok, resolve_model};
use ira::sdk::query::{open_session, query, QueryOptions};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc as StdArc;
use ira::pi::tool::ExecutionMode;
use ira::sdk::declarative::load_declarative_tools;
use ira::sdk::env::load_env;
use ira::sdk::loader::{discover_skills, discover_sub_agents, discover_workflows, load_agent};
use ira::sdk::manifest::PermissionConfig;
use ira::sdk::mcp::load_mcp_tools;
use ira::sdk::permissions::PermissionGate;
use ira::sdk::session::{init_local_session, RepoOptions};
use ira::sdk::telemetry::Telemetry;
use ira::sdk::tools::builtin_tools;
use serde_json::json;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio_util::sync::CancellationToken;

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Runtime::new().unwrap().block_on(f)
}

fn mutating_set() -> HashSet<String> {
    ["cli", "write", "edit", "memory"].into_iter().map(String::from).collect()
}

fn tmp_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let d = std::env::temp_dir().join(format!("gitagent-rs-{tag}-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&d).unwrap();
    d
}

fn run_git(args: &[&str], cwd: &Path) -> String {
    let out = Command::new("git").args(args).current_dir(cwd).output().unwrap();
    assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn find_sub(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Fully drain one HTTP request (headers + Content-Length body) so the client
/// finishes sending before we respond — avoids a broken-pipe on large requests.
fn drain_request(s: &mut TcpStream) {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
        let n = match s.read(&mut tmp) {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = find_sub(&buf, b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&buf[..pos]).to_lowercase();
            let clen = headers
                .lines()
                .find_map(|l| l.strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0)))
                .unwrap_or(0);
            let mut remaining = clen.saturating_sub(buf.len() - (pos + 4));
            while remaining > 0 {
                match s.read(&mut tmp) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => remaining = remaining.saturating_sub(n),
                }
            }
            return;
        }
    }
}

/// A mock OpenAI-compatible server. `handler(request_index) -> (status, sse_body)`.
/// Each connection is served on its own thread, so concurrent requests are
/// handled concurrently (lets timing tests observe real parallelism). Returns
/// the base URL and a request counter.
fn spawn_mock_llm(
    handler: impl Fn(usize) -> (u16, String) + Send + Sync + 'static,
) -> (String, StdArc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let count = StdArc::new(AtomicUsize::new(0));
    let counter = count.clone();
    let handler = StdArc::new(handler);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            let counter = counter.clone();
            let handler = handler.clone();
            std::thread::spawn(move || {
                drain_request(&mut s);
                let idx = counter.fetch_add(1, Ordering::SeqCst);
                let (status, body) = handler(idx);
                let reason = if status == 200 { "OK" } else { "Error" };
                let resp = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = s.write_all(resp.as_bytes());
                let _ = s.flush();
            });
        }
    });
    (format!("http://{addr}/v1"), count)
}

/// Read a full HTTP request (headers + Content-Length body) as a string.
fn read_full_request(s: &mut TcpStream) -> String {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
        let n = match s.read(&mut tmp) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = find_sub(&buf, b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&buf[..pos]).to_lowercase();
            let clen = headers
                .lines()
                .find_map(|l| l.strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0)))
                .unwrap_or(0);
            let mut remaining = clen.saturating_sub(buf.len() - (pos + 4));
            while remaining > 0 {
                match s.read(&mut tmp) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        buf.extend_from_slice(&tmp[..n]);
                        remaining = remaining.saturating_sub(n);
                    }
                }
            }
            break;
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

/// A mock server that records every request body (for asserting what was sent).
fn spawn_capturing_mock(reply: String) -> (String, StdArc<std::sync::Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let log = StdArc::new(std::sync::Mutex::new(Vec::new()));
    let sink = log.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            let req = read_full_request(&mut s);
            sink.lock().unwrap().push(req);
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                reply.len()
            );
            let _ = s.write_all(resp.as_bytes());
            let _ = s.flush();
        }
    });
    (format!("http://{addr}/v1"), log)
}

/// Drive a QueryStream to completion, returning the concatenated text deltas.
fn drain_query(mut opts: QueryOptions) -> String {
    opts.repo = None;
    block_on(async move {
        let mut stream = query(opts).unwrap();
        let mut text = String::new();
        while let Some(ev) = stream.next().await {
            if let ira::sdk::query::Event::Delta(t) = ev {
                text.push_str(&t);
            }
        }
        text
    })
}

#[test]
fn resolve_model_parses_at_base_url() {
    std::env::remove_var("GITAGENT_MODEL_BASE_URL");
    let s = resolve_model("openai:gpt-4o-mini@http://localhost:8090/v1");
    assert_eq!(s.model_id, "gpt-4o-mini");
    assert_eq!(s.base_url, "http://localhost:8090/v1");
}

#[test]
fn resolve_model_ollama_defaults_to_localhost() {
    std::env::remove_var("GITAGENT_MODEL_BASE_URL");
    let s = resolve_model("ollama:gemma3:4b");
    assert_eq!(s.model_id, "gemma3:4b"); // model tag with a colon is preserved
    assert_eq!(s.base_url, "http://localhost:11434/v1");
}

#[test]
fn resolve_model_defaults_to_openai() {
    std::env::remove_var("GITAGENT_MODEL_BASE_URL");
    let s = resolve_model("openai:gpt-4o");
    assert_eq!(s.model_id, "gpt-4o");
    assert_eq!(s.base_url, "https://api.openai.com/v1");
}

#[test]
fn loader_parses_manifest_and_builds_system_prompt() {
    let dir = tmp_dir("loader");
    fs::write(
        dir.join("agent.yaml"),
        r#"
spec_version: "0.1.0"
name: test-agent
description: A test agent.
model:
  preferred: "openai:gpt-4o-mini"
  fallback: []
tools: [cli, read, write, memory]
runtime:
  max_turns: 7
"#,
    )
    .unwrap();
    fs::write(dir.join("SOUL.md"), "Be terse.").unwrap();

    let loaded = load_agent(&dir).unwrap();
    assert_eq!(loaded.manifest.name, "test-agent");
    assert_eq!(loaded.manifest.max_turns(), 7);
    assert!(loaded.system_prompt.contains("You are test-agent."));
    assert!(loaded.system_prompt.contains("Be terse."));
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn builtin_tools_have_correct_execution_modes() {
    // Mutating tools must be Sequential (so the loop serializes them); read is Parallel.
    let tools = builtin_tools(Path::new("/tmp"));
    let mode = |n: &str| tools.iter().find(|t| t.name() == n).unwrap().execution_mode();
    assert_eq!(mode("cli"), ExecutionMode::Sequential);
    assert_eq!(mode("write"), ExecutionMode::Sequential);
    assert_eq!(mode("memory"), ExecutionMode::Sequential);
    assert_eq!(mode("read"), ExecutionMode::Parallel);
}

#[test]
fn compaction_gets_under_budget_without_dropping_messages() {
    let big = "x".repeat(100_000);
    let msgs = vec![
        AgentMessage::User("do the task".into()),
        AgentMessage::ToolResult(ToolResultMessage {
            tool_call_id: "1".into(),
            tool_name: "read".into(),
            content: big,
            is_error: false,
        }),
    ];
    let c = Compactor::new(2000); // budget ≈ 1500 tokens
    assert!(Compactor::estimate_tokens(&msgs) > c.max_tokens);
    let out = c.apply(&msgs);
    assert!(Compactor::estimate_tokens(&out) <= c.max_tokens, "must be under budget");
    // No message dropped → no orphaned tool_use/tool_result pairing.
    assert_eq!(out.len(), msgs.len());
    // Under-budget transcripts are returned unchanged.
    let small = vec![AgentMessage::User("hi".into())];
    assert_eq!(Compactor::new(128_000).apply(&small).len(), 1);
}

#[test]
fn context_window_sized_per_model() {
    assert_eq!(model_context_window("claude-haiku-4-5"), 200_000);
    assert_eq!(model_context_window("gpt-4o-mini"), 128_000);
    assert_eq!(model_context_window("gemini-1.5-pro"), 1_000_000);
}

#[test]
fn compaction_summarizes_old_turns_and_keeps_recent() {
    std::env::set_var("OPENAI_API_KEY", "test");
    // The summarizer model returns a known summary.
    let (base, count) = spawn_mock_llm(|_| {
        (200, "data: {\"choices\":[{\"delta\":{\"content\":\"SUMMARY: earlier work done\"}}]}\n\ndata: [DONE]\n\n".to_string())
    });
    let spec = resolve_model(&format!("openai:m@{base}"));

    // Over-budget transcript: 6 big tool results (old) + a small recent message.
    let big = "x".repeat(20_000);
    let mut msgs = vec![AgentMessage::User("start the task".into())];
    for i in 0..6 {
        msgs.push(AgentMessage::Assistant(AssistantMessage {
            content: vec![ContentBlock::ToolCall { id: format!("t{i}"), name: "read".into(), arguments: json!({"path": "f"}) }],
            stop_reason: StopReason::ToolUse,
            error_message: None,
            usage: Usage::default(),
        }));
        msgs.push(AgentMessage::ToolResult(ToolResultMessage {
            tool_call_id: format!("t{i}"),
            tool_name: "read".into(),
            content: big.clone(),
            is_error: false,
        }));
    }
    msgs.push(AgentMessage::User("the most recent instruction".into()));

    let c = Compactor::new(4000);
    assert!(Compactor::estimate_tokens(&msgs) > c.max_tokens);

    let out = block_on(c.compact(&reqwest::Client::new(), Some(&spec), &msgs, &CancellationToken::new()));
    assert!(Compactor::estimate_tokens(&out) <= c.max_tokens, "compacted under budget");
    assert!(matches!(&out[0], AgentMessage::User(s) if s.contains("SUMMARY")), "head folded into a summary");
    assert!(out.iter().any(|m| matches!(m, AgentMessage::User(s) if s.contains("most recent"))), "recent tail kept verbatim");
    assert!(count.load(Ordering::SeqCst) >= 1, "summarizer model was called");
}

#[test]
fn discovers_sub_agents_and_wires_run_agent() {
    let dir = tmp_dir("sub");
    fs::create_dir_all(dir.join("agents/helper")).unwrap();
    fs::write(dir.join("agents/helper/agent.yaml"), "name: helper\nmodel:\n  preferred: \"openai:gpt-4o-mini\"\n").unwrap();
    assert_eq!(discover_sub_agents(&dir), vec!["helper".to_string()]);
    assert!(builtin_tools(&dir).iter().any(|t| t.name() == "run_agent"));
    let empty = tmp_dir("noagents");
    assert!(!builtin_tools(&empty).iter().any(|t| t.name() == "run_agent"));
    fs::remove_dir_all(&dir).ok();
    fs::remove_dir_all(&empty).ok();
}

#[test]
fn discovers_skills_and_injects_into_prompt() {
    let dir = tmp_dir("skills");
    fs::create_dir_all(dir.join("skills/coder")).unwrap();
    fs::write(dir.join("skills/coder/SKILL.md"), "---\nname: coder\ndescription: writes code\n---\n# Coder\n").unwrap();
    fs::write(dir.join("agent.yaml"), "name: a\nmodel:\n  preferred: \"openai:gpt-4o\"\n").unwrap();
    assert_eq!(discover_skills(&dir), vec![("coder".to_string(), "writes code".to_string())]);
    let loaded = load_agent(&dir).unwrap();
    assert!(loaded.system_prompt.contains("# Skills"));
    assert!(loaded.system_prompt.contains("coder: writes code"));
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn repo_mode_clones_branches_and_pushes() {
    for (k, v) in [
        ("GIT_AUTHOR_NAME", "t"), ("GIT_AUTHOR_EMAIL", "t@t"),
        ("GIT_COMMITTER_NAME", "t"), ("GIT_COMMITTER_EMAIL", "t@t"),
    ] {
        std::env::set_var(k, v);
    }
    let base = tmp_dir("repo");
    let remote = base.join("remote.git");
    let seed = base.join("seed");
    let clone = base.join("clone");

    run_git(&["-c", "init.defaultBranch=main", "init", "--bare", remote.to_str().unwrap()], &base);
    fs::create_dir_all(&seed).unwrap();
    run_git(&["-c", "init.defaultBranch=main", "init"], &seed);
    fs::write(seed.join("README.md"), "hi").unwrap();
    run_git(&["add", "-A"], &seed);
    run_git(&["commit", "-m", "init"], &seed);
    let remote_url = format!("file://{}", remote.display());
    run_git(&["remote", "add", "origin", &remote_url], &seed);
    run_git(&["push", "origin", "main"], &seed);

    let s = init_local_session(RepoOptions {
        url: remote_url.clone(),
        token: String::new(),
        dir: clone.clone(),
        session: None,
        trust_repo: false,
    })
    .unwrap();

    assert!(s.branch.starts_with("gitagent/session-"), "branch was {}", s.branch);
    assert!(clone.join("agent.yaml").exists(), "agent.yaml scaffolded");
    s.finalize().unwrap();

    let refs = run_git(&["ls-remote", &remote_url], &base);
    assert!(refs.contains(&s.branch), "session branch pushed to remote:\n{refs}");

    fs::remove_dir_all(&base).ok();
}

fn cfg(mode: &str, allow: &[&str], deny: &[&str]) -> PermissionConfig {
    PermissionConfig {
        mode: mode.to_string(),
        allow: allow.iter().map(|s| s.to_string()).collect(),
        deny: deny.iter().map(|s| s.to_string()).collect(),
    }
}

#[test]
fn permission_gate_modes_and_rules() {
    let denied = |g: &PermissionGate, name: &str, args: serde_json::Value| {
        matches!(block_on(g.check(name, &args)), GateDecision::Deny(_))
    };

    // Deny rule blocks that tool; others pass.
    let g = PermissionGate::new(&cfg("", &[], &["cli"]), mutating_set(), None);
    assert!(denied(&g, "cli", json!({"command": "ls"})));
    assert!(!denied(&g, "read", json!({"path": "x"})));

    // Plan mode blocks mutating tools; read-only passes; allow-rule overrides.
    let g = PermissionGate::new(&cfg("plan", &["write"], &[]), mutating_set(), None);
    assert!(denied(&g, "cli", json!({"command": "rm"})));
    assert!(!denied(&g, "read", json!({})));
    assert!(!denied(&g, "write", json!({}))); // allow-listed despite plan mode

    // Bypass allows everything — even a deny rule.
    let g = PermissionGate::new(&cfg("bypass", &[], &["cli"]), mutating_set(), None);
    assert!(!denied(&g, "cli", json!({"command": "rm -rf /"})));

    // CLI override wins over the manifest mode.
    let g = PermissionGate::new(&cfg("bypass", &[], &[]), mutating_set(), Some("plan"));
    assert!(denied(&g, "write", json!({})));

    // Pattern rule matches only the substring.
    let g = PermissionGate::new(&cfg("", &[], &["cli(rm )"]), mutating_set(), None);
    assert!(denied(&g, "cli", json!({"command": "rm -rf /"})));
    assert!(!denied(&g, "cli", json!({"command": "ls -la"})));

    // No rules + default mode = no-op gate (skipped entirely in query()).
    assert!(PermissionGate::new(&cfg("", &[], &[]), mutating_set(), None).is_noop());
}

#[test]
fn declarative_tools_load_and_execute() {
    let dir = tmp_dir("decl");
    fs::create_dir_all(dir.join("tools")).unwrap();
    fs::write(dir.join("tools/echo.yaml"), "name: echo\ndescription: echoes stdin\nrun: cat\nexecution: sequential\n").unwrap();

    let tools = load_declarative_tools(&dir);
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name(), "echo");
    assert_eq!(tools[0].execution_mode(), ExecutionMode::Sequential);

    let cancel = CancellationToken::new();
    let res = block_on(tools[0].execute("1", json!({"hello": "world"}), &cancel)).unwrap();
    assert!(res.content.contains("hello"), "script received args JSON: {}", res.content);
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn self_correcting_goal_loop_retries_until_verified() {
    use ira::sdk::goal::{run_goal, Goal};
    std::env::set_var("OPENAI_API_KEY", "test");

    // Attempt 1: the model does nothing (no file) → verify fails.
    // Attempt 2: it writes goal.txt (request 1), then stops (request 2) → verify passes.
    let stop = |t: &str| format!("data: {{\"choices\":[{{\"delta\":{{\"content\":\"{t}\"}}}}]}}\n\ndata: [DONE]\n\n");
    let write = "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"function\":{\"name\":\"write\",\"arguments\":\"{\\\"path\\\":\\\"goal.txt\\\",\\\"content\\\":\\\"done\\\"}\"}}]}}]}\n\ndata: [DONE]\n\n";
    let (base, _) = spawn_mock_llm(move |i| match i {
        0 => (200, stop("nothing yet")),      // attempt 1: no action
        1 => (200, write.to_string()),        // attempt 2: write the file
        _ => (200, stop("done")),             // attempt 2: finish
    });

    let dir = tmp_dir("goal");
    fs::write(dir.join("agent.yaml"), "name: a\nmodel:\n  preferred: \"openai:m\"\n").unwrap();

    let outcome = block_on(run_goal(
        &dir,
        Goal {
            prompt: "create goal.txt".into(),
            verify: Some("test -f goal.txt".into()),
            max_attempts: 3,
            model: Some(format!("openai:m@{base}")),
            reflect: false,
        },
    ))
    .unwrap();

    assert!(outcome.passed, "goal must eventually verify");
    assert_eq!(outcome.attempts, 2, "passes on the 2nd, self-corrected attempt");
    assert_eq!(outcome.lessons.len(), 1, "one failure lesson recorded from attempt 1");
    assert!(dir.join("goal.txt").exists());
    // Trajectories + lessons persisted for learning.
    assert!(dir.join(".gitagent/trajectories/attempt-1.json").exists());
    assert!(dir.join(".gitagent/lessons.md").exists());
    // Self-learning: the winning trajectory becomes a reinforced skill.
    let skill = fs::read_to_string(dir.join("skills/create-goal-txt/SKILL.md")).expect("success skill written");
    assert!(skill.contains("confidence:") && skill.contains("Winning steps"));
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn reflection_diagnoses_failure_and_feeds_next_attempt() {
    use ira::sdk::goal::{run_goal, Goal};
    std::env::set_var("OPENAI_API_KEY", "test");
    let stop = |t: &str| format!("data: {{\"choices\":[{{\"delta\":{{\"content\":\"{t}\"}}}}]}}\n\ndata: [DONE]\n\n");
    let write = "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"function\":{\"name\":\"write\",\"arguments\":\"{\\\"path\\\":\\\"goal.txt\\\",\\\"content\\\":\\\"done\\\"}\"}}]}}]}\n\ndata: [DONE]\n\n";
    let (base, _) = spawn_mock_llm(move |i| match i {
        0 => (200, stop("nothing yet")),                                                  // attempt 1
        1 => (200, stop("you forgot to call the write tool; create goal.txt with it")),   // reflection
        2 => (200, write.to_string()),                                                    // attempt 2
        _ => (200, stop("done")),                                                         // attempt 2 finish
    });

    let dir = tmp_dir("reflect");
    fs::write(dir.join("agent.yaml"), "name: a\nmodel:\n  preferred: \"openai:m\"\n").unwrap();

    let outcome = block_on(run_goal(
        &dir,
        Goal {
            prompt: "create goal.txt".into(),
            verify: Some("test -f goal.txt".into()),
            max_attempts: 3,
            model: Some(format!("openai:m@{base}")),
            reflect: true,
        },
    ))
    .unwrap();

    assert!(outcome.passed && outcome.attempts == 2);
    assert!(
        outcome.lessons[0].contains("write tool"),
        "the reflection (not raw verify output) became the correction: {:?}",
        outcome.lessons[0]
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn session_carries_context_across_turns() {
    std::env::set_var("OPENAI_API_KEY", "test");
    let reply = "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\ndata: [DONE]\n\n".to_string();
    let (base, log) = spawn_capturing_mock(reply);

    let dir = tmp_dir("session");
    fs::write(dir.join("agent.yaml"), format!("name: a\nmodel:\n  preferred: \"openai:m@{base}\"\n")).unwrap();

    let session = open_session(dir.clone(), None, None, None).unwrap();
    block_on(async {
        let mut t1 = session.send("remember the codeword bluebird");
        while t1.next().await.is_some() {}
        let mut t2 = session.send("what was the codeword");
        while t2.next().await.is_some() {}
    });

    let reqs = log.lock().unwrap();
    assert_eq!(reqs.len(), 2, "two turns → two requests");
    // The 2nd request must carry the 1st turn's message — proves the transcript
    // persisted AND was committed before the next turn (no stale-read race).
    assert!(reqs[1].contains("bluebird"), "2nd turn missing 1st turn context");
    assert!(reqs[1].contains("what was the codeword"));
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn subagents_run_in_parallel() {
    std::env::set_var("OPENAI_API_KEY", "test");
    // Sub-agent model: each call takes ~500ms then replies.
    let (base_s, _) = spawn_mock_llm(|_| {
        std::thread::sleep(Duration::from_millis(500));
        (200, "data: {\"choices\":[{\"delta\":{\"content\":\"done\"}}]}\n\ndata: [DONE]\n\n".to_string())
    });
    // Parent model: turn 1 asks for TWO sub-agents at once; turn 2 stops. (instant)
    let batch = "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"function\":{\"name\":\"run_agent\",\"arguments\":\"{\\\"agent\\\":\\\"a\\\",\\\"prompt\\\":\\\"do a\\\"}\"}},{\"index\":1,\"id\":\"c2\",\"function\":{\"name\":\"run_agent\",\"arguments\":\"{\\\"agent\\\":\\\"b\\\",\\\"prompt\\\":\\\"do b\\\"}\"}}]}}]}\n\ndata: [DONE]\n\n".to_string();
    let (base_p, _) = spawn_mock_llm(move |i| {
        if i == 0 { (200, batch.clone()) } else { (200, "data: {\"choices\":[{\"delta\":{\"content\":\"finished\"}}]}\n\ndata: [DONE]\n\n".to_string()) }
    });

    let dir = tmp_dir("subpar");
    fs::create_dir_all(dir.join("agents/a")).unwrap();
    fs::create_dir_all(dir.join("agents/b")).unwrap();
    fs::write(dir.join("agents/a/agent.yaml"), format!("name: a\nmodel:\n  preferred: \"openai:m@{base_s}\"\n")).unwrap();
    fs::write(dir.join("agents/b/agent.yaml"), format!("name: b\nmodel:\n  preferred: \"openai:m@{base_s}\"\n")).unwrap();
    fs::write(dir.join("agent.yaml"), "name: parent\nmodel:\n  preferred: \"openai:m\"\n").unwrap();

    let start = Instant::now();
    drain_query(QueryOptions {
        dir: dir.clone(),
        model: Some(format!("openai:m@{base_p}")),
        prompt: "delegate to both".into(),
        repo: None,
        permission_mode: None,
    });
    let elapsed = start.elapsed();
    // Two 500ms sub-agents in parallel ≈ 500ms; sequential would be ≥ 1000ms.
    assert!(elapsed < Duration::from_millis(850), "sub-agents must run in parallel; took {elapsed:?}");
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn goal_move_advances_through_a_sequence() {
    use ira::sdk::goal::{run_goals, Goal};
    std::env::set_var("OPENAI_API_KEY", "test");
    // The model just says "ok"; each goal's verify command is `true` (passes).
    let stop = "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\ndata: [DONE]\n\n";
    let (base, count) = spawn_mock_llm(move |_| (200, stop.to_string()));

    let dir = tmp_dir("goalmove");
    fs::write(dir.join("agent.yaml"), "name: a\nmodel:\n  preferred: \"openai:m\"\n").unwrap();
    let mk = |p: &str| Goal { prompt: p.into(), verify: Some("true".into()), max_attempts: 2, model: Some(format!("openai:m@{base}")), reflect: false };

    let outcomes = block_on(run_goals(&dir, vec![mk("goal one"), mk("goal two"), mk("goal three")])).unwrap();
    assert_eq!(outcomes.len(), 3, "advanced through all three goals");
    assert!(outcomes.iter().all(|o| o.passed));
    assert!(count.load(Ordering::SeqCst) >= 3, "each goal ran at least one attempt");
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn transient_error_classification() {
    for m in ["provider error 429: rate limit", "connection reset", "503 Service Unavailable", "request timed out"] {
        assert!(is_transient_error(m), "should be transient: {m}");
    }
    for m in ["provider error 400: bad request", "invalid api key", "model not found"] {
        assert!(!is_transient_error(m), "should NOT be transient: {m}");
    }
}

#[test]
fn empty_assistant_turns_serialize_content_as_string() {
    std::env::set_var("OPENAI_API_KEY", "test");
    // Turn 1: the model emits ONLY reasoning (no text) → empty assistant message.
    let reply = "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"hmm\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n".to_string();
    let (base, log) = spawn_capturing_mock(reply);

    let dir = tmp_dir("nilcontent");
    fs::write(dir.join("agent.yaml"), format!("name: a\nmodel:\n  preferred: \"openai:m@{base}\"\n")).unwrap();

    let session = open_session(dir.clone(), None, None, None).unwrap();
    block_on(async {
        let mut t1 = session.send("think about it");
        while t1.next().await.is_some() {}
        let mut t2 = session.send("now answer");
        while t2.next().await.is_some() {}
    });

    let reqs = log.lock().unwrap();
    assert_eq!(reqs.len(), 2);
    // The replayed empty assistant turn must carry content:"" — a missing
    // content field makes Ollama 400 with `invalid message content type: <nil>`.
    let msgs_part: String = reqs[1].chars().skip(reqs[1].find("\"messages\"").unwrap_or(0)).take(600).collect();
    assert!(reqs[1].contains(r#"{"content":"","role":"assistant"}"#),
        "assistant content must be a string; messages were: {msgs_part}");
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn auto_continues_after_length_cut() {
    std::env::set_var("OPENAI_API_KEY", "test");
    // Request 1: output cut by max_tokens (finish_reason "length").
    // Request 2: the remainder, finishing normally.
    let part1 = "data: {\"choices\":[{\"delta\":{\"content\":\"function half() {\"},\"finish_reason\":\"length\"}]}\n\ndata: [DONE]\n\n";
    let part2 = "data: {\"choices\":[{\"delta\":{\"content\":\" return 42; }\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
    let (base, count) = spawn_mock_llm(move |i| (200, if i == 0 { part1.to_string() } else { part2.to_string() }));

    let dir = tmp_dir("autocont");
    fs::write(dir.join("agent.yaml"), "name: a\nmodel:\n  preferred: \"openai:m\"\n").unwrap();

    let text = drain_query(QueryOptions {
        dir: dir.clone(),
        model: Some(format!("openai:m@{base}")),
        prompt: "write the function".into(),
        repo: None,
        permission_mode: None,
    });
    assert!(text.contains("function half() {") && text.contains("return 42; }"),
        "both halves streamed seamlessly: {text:?}");
    assert_eq!(count.load(Ordering::SeqCst), 2, "auto-continued exactly once");
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn max_turns_caps_the_loop() {
    std::env::set_var("OPENAI_API_KEY", "test");
    // The model always asks for another tool call → the loop would run forever
    // without the cap. runtime.max_turns=3 must stop it after exactly 3 turns.
    let sse = "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"function\":{\"name\":\"read\",\"arguments\":\"{\\\"path\\\":\\\"/nope\\\"}\"}}]}}]}\n\ndata: [DONE]\n\n";
    let (base, count) = spawn_mock_llm(move |_| (200, sse.to_string()));

    let dir = tmp_dir("maxturns");
    fs::write(dir.join("agent.yaml"), "name: a\nmodel:\n  preferred: \"openai:m\"\nruntime:\n  max_turns: 3\n").unwrap();

    drain_query(QueryOptions {
        dir: dir.clone(),
        model: Some(format!("openai:m@{base}")),
        prompt: "go".into(),
        repo: None,
        permission_mode: None,
    });
    assert_eq!(count.load(Ordering::SeqCst), 3, "loop must stop after max_turns requests");
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn falls_back_to_second_model_on_error() {
    std::env::set_var("OPENAI_API_KEY", "test");
    // Preferred model returns a non-transient 400 → immediate fallback.
    let (base_a, count_a) = spawn_mock_llm(|_| (400, "bad request".to_string()));
    let text_sse = "data: {\"choices\":[{\"delta\":{\"content\":\"hello from fallback\"}}]}\n\ndata: [DONE]\n\n";
    let (base_b, count_b) = spawn_mock_llm(move |_| (200, text_sse.to_string()));

    let dir = tmp_dir("fallback");
    fs::write(
        dir.join("agent.yaml"),
        format!("name: a\nmodel:\n  preferred: \"openai:m@{base_a}\"\n  fallback: [\"openai:m@{base_b}\"]\n"),
    )
    .unwrap();

    let text = drain_query(QueryOptions {
        dir: dir.clone(),
        model: None,
        prompt: "go".into(),
        repo: None,
        permission_mode: None,
    });
    assert!(text.contains("hello from fallback"), "used fallback model's output: {text:?}");
    assert_eq!(count_a.load(Ordering::SeqCst), 1, "preferred tried once");
    assert!(count_b.load(Ordering::SeqCst) >= 1, "fallback used");
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn recovers_text_tool_call_from_local_model() {
    use ira::pi::message::{ContentBlock, StopReason};
    use ira::pi::provider::{stream_assistant, GenParams};
    std::env::set_var("OPENAI_API_KEY", "test");
    // A Gemma-style fenced JSON tool call arriving as plain content.
    let inner = "```json\n{\"name\": \"read\", \"arguments\": {\"path\": \"x\"}}\n```";
    let body = format!("data: {}\n\ndata: [DONE]\n\n", json!({"choices":[{"delta":{"content": inner}}]}));
    let (base, _) = spawn_mock_llm(move |_| (200, body.clone()));
    let spec = resolve_model(&format!("openai:m@{base}"));
    let tools = builtin_tools(Path::new("/tmp")); // non-empty → enables the fallback
    let cancel = CancellationToken::new();

    let msg = block_on(async {
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
        let m = stream_assistant(&reqwest::Client::new(), &spec, "sys", &[], &tools, &GenParams::default(), &cancel, &tx).await;
        drop(tx);
        let _ = drain.await;
        m
    });

    let tc = msg.content.iter().find_map(|b| match b {
        ContentBlock::ToolCall { name, arguments, .. } => Some((name.clone(), arguments.clone())),
        _ => None,
    });
    assert_eq!(tc.as_ref().map(|(n, _)| n.as_str()), Some("read"), "text tool call recovered");
    assert_eq!(tc.unwrap().1.get("path").and_then(|v| v.as_str()), Some("x"));
    assert!(matches!(msg.stop_reason, StopReason::ToolUse));
}

#[test]
fn muse_channels_split_into_thinking_and_answer() {
    use ira::pi::message::{ContentBlock, StopReason};
    use ira::pi::provider::{stream_assistant, GenParams};
    std::env::set_var("OPENAI_API_KEY", "test");
    // Muse-style dual-channel output arriving as plain content chunks.
    let full = " to=self<|message|>User wants a greeting. Keep it short.<|start|>assistant to=user<|message|>Hello there!";
    let (a, b) = full.split_at(30); // split mid-marker region to test buffering
    let sse = format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        json!({"choices":[{"delta":{"content": a}}]}),
        json!({"choices":[{"delta":{"content": b}}]}),
    );
    let (base, _) = spawn_mock_llm(move |_| (200, sse.clone()));
    let spec = resolve_model(&format!("ollama:muse-glimmer:q2@{base}"));
    let cancel = CancellationToken::new();

    let msg = block_on(async {
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
        let m = stream_assistant(&reqwest::Client::new(), &spec, "sys", &[], &[], &GenParams::default(), &cancel, &tx).await;
        drop(tx);
        let _ = drain.await;
        m
    });

    let think = msg.content.iter().find_map(|c| match c { ContentBlock::Thinking(t) => Some(t.clone()), _ => None }).unwrap_or_default();
    assert!(think.contains("wants a greeting"), "reasoning captured: {think:?}");
    assert!(!think.contains("<|"), "markers stripped from reasoning");
    assert_eq!(msg.text().trim(), "Hello there!", "answer channel isolated");
    assert!(matches!(msg.stop_reason, StopReason::Stop));
}

#[test]
fn muse_short_plain_answer_not_swallowed() {
    use ira::pi::message::{ContentBlock, StopReason};
    use ira::pi::provider::{stream_assistant, GenParams};
    std::env::set_var("OPENAI_API_KEY", "test");
    // Official-template Muse: reasoning arrives in the `reasoning` field and the
    // answer as SHORT plain content (<20 chars, no channel markers).
    let sse = format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        json!({"choices":[{"delta":{"reasoning":"user wants a greeting"}}]}),
        json!({"choices":[{"delta":{"content":"hello world"},"finish_reason":"stop"}]}),
    );
    let (base, _) = spawn_mock_llm(move |_| (200, sse.clone()));
    let spec = resolve_model(&format!("ollama:muse-glimmer:30b@{base}"));
    let cancel = CancellationToken::new();

    let msg = block_on(async {
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
        let m = stream_assistant(&reqwest::Client::new(), &spec, "sys", &[], &[], &GenParams::default(), &cancel, &tx).await;
        drop(tx);
        let _ = drain.await;
        m
    });

    assert_eq!(msg.text().trim(), "hello world", "short answer must never be swallowed");
    let think = msg.content.iter().any(|c| matches!(c, ContentBlock::Thinking(t) if t.contains("greeting")));
    assert!(think, "native reasoning field captured");
    assert!(matches!(msg.stop_reason, StopReason::Stop));
}

#[test]
fn captures_reasoning_and_length_finish() {
    use ira::pi::message::{ContentBlock, StopReason};
    use ira::pi::provider::{stream_assistant, GenParams};
    std::env::set_var("OPENAI_API_KEY", "test");
    // Reasoning delta, then an answer truncated by length.
    let sse = "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"thinking hard\"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"the answer\"},\"finish_reason\":\"length\"}]}\n\ndata: [DONE]\n\n";
    let (base, _) = spawn_mock_llm(move |_| (200, sse.to_string()));
    let spec = resolve_model(&format!("openai:m@{base}"));
    let cancel = CancellationToken::new();

    let msg = block_on(async {
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
        let m = stream_assistant(&reqwest::Client::new(), &spec, "sys", &[], &[], &GenParams::default(), &cancel, &tx).await;
        drop(tx);
        let _ = drain.await;
        m
    });

    assert!(matches!(msg.stop_reason, StopReason::Length), "length finish_reason honored");
    assert!(
        msg.content.iter().any(|b| matches!(b, ContentBlock::Thinking(t) if t.contains("thinking"))),
        "reasoning captured as a Thinking block"
    );
    assert!(msg.text().contains("the answer"));
}

#[test]
fn cost_table_maps_models() {
    assert_eq!(model_cost_per_mtok("openai/gpt-4o-mini"), (0.15, 0.60));
    assert_eq!(model_cost_per_mtok("gpt-4o"), (2.5, 10.0));
    assert_eq!(model_cost_per_mtok("claude-3-5-sonnet"), (3.0, 15.0));
    assert_eq!(model_cost_per_mtok("some-unknown-model"), (0.0, 0.0));
}

#[test]
fn env_file_loads_into_process_env() {
    let dir = tmp_dir("env");
    fs::write(dir.join(".env"), "GITAGENT_TEST_KEY_XYZ=\"hello world\"\n# a comment\nEMPTYLINE_OK=1\n").unwrap();
    load_env(&dir);
    assert_eq!(std::env::var("GITAGENT_TEST_KEY_XYZ").unwrap(), "hello world");
    assert_eq!(std::env::var("EMPTYLINE_OK").unwrap(), "1");
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn discovers_workflows_and_injects() {
    let dir = tmp_dir("wf");
    fs::create_dir_all(dir.join("workflows")).unwrap();
    fs::write(dir.join("workflows/deploy.md"), "---\nname: deploy\ndescription: ship it\n---\n# steps\n").unwrap();
    fs::write(dir.join("agent.yaml"), "name: a\nmodel:\n  preferred: \"openai:gpt-4o\"\n").unwrap();
    assert_eq!(discover_workflows(&dir), vec![("deploy".to_string(), "ship it".to_string())]);
    let loaded = load_agent(&dir).unwrap();
    assert!(loaded.system_prompt.contains("# Workflows"));
    assert!(loaded.system_prompt.contains("deploy: ship it"));
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn extends_inherits_parent_identity_before_child() {
    let base = tmp_dir("ext");
    let parent = base.join("parent");
    let child = base.join("child");
    fs::create_dir_all(&parent).unwrap();
    fs::create_dir_all(&child).unwrap();
    fs::write(parent.join("agent.yaml"), "name: parent\nmodel:\n  preferred: \"openai:gpt-4o\"\n").unwrap();
    fs::write(parent.join("SOUL.md"), "Parent soul.").unwrap();
    fs::write(child.join("agent.yaml"), "name: child\nextends: ../parent\nmodel:\n  preferred: \"openai:gpt-4o\"\n").unwrap();
    fs::write(child.join("SOUL.md"), "Child soul.").unwrap();

    let prompt = load_agent(&child).unwrap().system_prompt;
    assert!(prompt.contains("# Inherited SOUL"));
    let pi = prompt.find("Parent soul.").expect("parent soul present");
    let ci = prompt.find("Child soul.").expect("child soul present");
    assert!(pi < ci, "inherited identity must precede the child's own");
    fs::remove_dir_all(&base).ok();
}

#[test]
fn extends_clones_parent_from_git_url() {
    for (k, v) in [
        ("GIT_AUTHOR_NAME", "t"), ("GIT_AUTHOR_EMAIL", "t@t"),
        ("GIT_COMMITTER_NAME", "t"), ("GIT_COMMITTER_EMAIL", "t@t"),
    ] {
        std::env::set_var(k, v);
    }
    let base = tmp_dir("extgit");
    let remote = base.join("parent.git");
    let seed = base.join("seed");
    let child = base.join("child");

    // Bare "parent" repo carrying an agent identity.
    run_git(&["-c", "init.defaultBranch=main", "init", "--bare", remote.to_str().unwrap()], &base);
    fs::create_dir_all(&seed).unwrap();
    run_git(&["-c", "init.defaultBranch=main", "init"], &seed);
    fs::write(seed.join("agent.yaml"), "name: parent\nmodel:\n  preferred: \"openai:gpt-4o\"\n").unwrap();
    fs::write(seed.join("SOUL.md"), "Parent-from-git soul.").unwrap();
    run_git(&["add", "-A"], &seed);
    run_git(&["commit", "-m", "init"], &seed);
    let remote_url = format!("file://{}", remote.display());
    run_git(&["remote", "add", "origin", &remote_url], &seed);
    run_git(&["push", "origin", "main"], &seed);

    // Child extends the parent by git URL.
    fs::create_dir_all(&child).unwrap();
    fs::write(
        child.join("agent.yaml"),
        format!("name: child\nextends: {remote_url}\nmodel:\n  preferred: \"openai:gpt-4o\"\n"),
    )
    .unwrap();
    fs::write(child.join("SOUL.md"), "Child soul.").unwrap();

    let prompt = load_agent(&child).unwrap().system_prompt;
    assert!(prompt.contains("# Inherited SOUL"));
    assert!(prompt.contains("Parent-from-git soul."), "parent cloned + injected");
    // The clone landed under .gitagent/deps/<name>.
    assert!(child.join(".gitagent/deps/parent/agent.yaml").exists(), "cloned into .gitagent/deps/");
    fs::remove_dir_all(&base).ok();
}

#[test]
fn compliance_injected_into_prompt() {
    let dir = tmp_dir("comp");
    fs::write(
        dir.join("agent.yaml"),
        "name: a\nmodel:\n  preferred: \"openai:gpt-4o\"\ncompliance:\n  standards: [SOC2]\n  rules:\n    - never exfiltrate secrets\n",
    )
    .unwrap();
    let prompt = load_agent(&dir).unwrap().system_prompt;
    assert!(prompt.contains("# Compliance"));
    assert!(prompt.contains("SOC2"));
    assert!(prompt.contains("never exfiltrate secrets"));
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn read_tool_extracts_docx_and_pdf_text() {
    use std::io::Write as _;
    let dir = tmp_dir("extract");

    // A minimal real .docx (zip with word/document.xml).
    let docx_path = dir.join("roadmap.docx");
    {
        let f = fs::File::create(&docx_path).unwrap();
        let mut z = zip::ZipWriter::new(f);
        let opts = zip::write::SimpleFileOptions::default();
        z.start_file("word/document.xml", opts).unwrap();
        z.write_all(br#"<?xml version="1.0"?><w:document><w:body><w:p><w:r><w:t>Q3 Roadmap: ship Lyzr blocks</w:t></w:r></w:p><w:p><w:r><w:t>Phase two follows.</w:t></w:r></w:p></w:body></w:document>"#).unwrap();
        z.finish().unwrap();
    }
    // A real PDF from our own writer.
    fs::write(dir.join("doc.pdf"), ira::sdk::pdf::build_pdf(Some("Plan"), "Alpha beta gamma.")).unwrap();

    let tools = builtin_tools(&dir);
    let read = tools.iter().find(|t| t.name() == "read").unwrap();
    let cancel = CancellationToken::new();

    let d = block_on(read.execute("1", json!({"path":"roadmap.docx"}), &cancel)).unwrap();
    assert!(d.content.contains("Q3 Roadmap: ship Lyzr blocks"), "docx text extracted: {}", &d.content[..d.content.len().min(200)]);
    assert!(d.content.contains("Phase two follows."));

    let p = block_on(read.execute("2", json!({"path":"doc.pdf"}), &cancel)).unwrap();
    assert!(p.content.contains("Alpha beta gamma"), "pdf text extracted: {}", &p.content[..p.content.len().min(200)]);
    assert!(p.content.contains("Plan"));
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn pdf_tool_writes_valid_multipage_pdf() {
    use ira::sdk::pdf::build_pdf;
    let dir = tmp_dir("pdf");
    // Unit: structure of the generated bytes.
    let long_body = (0..120).map(|i| format!("Paragraph {i} with enough words to wrap across the page width nicely.")).collect::<Vec<_>>().join("\n\n");
    let bytes = build_pdf(Some("Test Doc"), &format!("# Heading\n\n- bullet one\n- bullet two\n\n{long_body}"));
    assert!(bytes.starts_with(b"%PDF-1.4"), "valid header");
    assert!(bytes.windows(6).any(|w| w == b"%%EOF\n"), "valid trailer");
    let s = String::from_utf8_lossy(&bytes);
    let count: u32 = s.split("/Count ").nth(1).and_then(|r| r.split('>').next()).and_then(|n| n.trim().parse().ok()).unwrap_or(0);
    assert!(count >= 2, "long content paginates (got {count} pages)");
    assert!(s.contains("Helvetica-Bold"), "headings use bold font");

    // Tool: writes the file via the agent tool interface.
    let tools = builtin_tools(&dir);
    let pdf = tools.iter().find(|t| t.name() == "pdf").expect("pdf tool registered");
    let cancel = CancellationToken::new();
    let res = block_on(pdf.execute("1", json!({"path":"out.pdf","title":"Hi","content":"Hello world"}), &cancel)).unwrap();
    assert!(res.content.contains("Wrote PDF"), "{}", res.content);
    let written = fs::read(dir.join("out.pdf")).unwrap();
    assert!(written.starts_with(b"%PDF-1.4"));
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn sandbox_wrapper_wraps_cli_execution() {
    let dir = tmp_dir("sbx");
    // Wrapper injects an env var set to {cwd}; the command reads it back — proving
    // both that the wrapper ran and that {cwd} was substituted.
    fs::write(
        dir.join("agent.yaml"),
        "name: a\nmodel:\n  preferred: \"openai:gpt-4o\"\nsandbox:\n  wrapper: [\"env\", \"SBX={cwd}\", \"sh\", \"-c\"]\n",
    )
    .unwrap();
    let tools = builtin_tools(&dir);
    let cli = tools.iter().find(|t| t.name() == "cli").unwrap();
    let cancel = CancellationToken::new();
    let res = block_on(cli.execute("1", json!({"command": "echo SBX=$SBX"}), &cancel)).unwrap();
    assert!(res.content.contains("SBX="), "wrapper ran: {}", res.content);
    assert!(res.content.contains(&dir.display().to_string()), "{{cwd}} substituted: {}", res.content);
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn telemetry_records_when_enabled() {
    let dir = tmp_dir("tel");
    std::env::set_var("GITAGENT_TELEMETRY", "1");
    let t = Telemetry::new(&dir).expect("enabled via env");
    t.record("tool_call", json!({"name": "cli"}));
    std::env::remove_var("GITAGENT_TELEMETRY");
    // Disabled without the env var.
    assert!(Telemetry::new(&dir).is_none());

    let content = fs::read_to_string(dir.join(".gitagent/telemetry.jsonl")).unwrap();
    assert!(content.contains("tool_call") && content.contains("cli"));
    fs::remove_dir_all(&dir).ok();
}

// A minimal MCP server over stdio: speaks just enough JSON-RPC for the
// handshake, one tool, and one call.
const MOCK_MCP_SERVER: &str = r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"initialize"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2024-11-05","capabilities":{},"serverInfo":{"name":"mock","version":"1"}}}\n' "$id" ;;
    *'notifications/initialized'*)
      : ;;
    *'tools/list'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"echo","description":"echoes msg","inputSchema":{"type":"object","properties":{"msg":{"type":"string"}}}}]}}\n' "$id" ;;
    *'tools/call'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"pong"}]}}\n' "$id" ;;
  esac
done
"#;

#[test]
fn mcp_stdio_client_lists_and_calls_tools() {
    let dir = tmp_dir("mcp");
    let mock = dir.join("mock.sh");
    fs::write(&mock, MOCK_MCP_SERVER).unwrap();
    fs::write(
        dir.join("mcp.yaml"),
        format!("servers:\n  - name: mock\n    command: \"sh {}\"\n", mock.display()),
    )
    .unwrap();

    let tools = load_mcp_tools(&dir);
    assert_eq!(tools.len(), 1, "one tool from the mock server");
    assert_eq!(tools[0].name(), "mcp__mock__echo");

    let cancel = CancellationToken::new();
    let res = block_on(tools[0].execute("1", json!({"msg": "hi"}), &cancel)).unwrap();
    assert_eq!(res.content, "pong");
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn learning_tools_track_and_reinforce() {
    let dir = tmp_dir("learn");
    let tools = builtin_tools(&dir);
    let tt = tools.iter().find(|t| t.name() == "task_tracker").unwrap();
    let sl = tools.iter().find(|t| t.name() == "skill_learner").unwrap();
    let cancel = CancellationToken::new();

    // task tracker: add → list → complete.
    block_on(tt.execute("1", json!({"action": "add", "text": "write tests"}), &cancel)).unwrap();
    let listed = block_on(tt.execute("2", json!({"action": "list"}), &cancel)).unwrap();
    assert!(listed.content.contains("write tests") && listed.content.contains("#1"));
    let done = block_on(tt.execute("3", json!({"action": "complete", "id": 1}), &cancel)).unwrap();
    assert!(done.content.contains("[x] #1"));

    // skill learner: save (confidence 0.5) → reinforce (+0.3 → 0.80).
    block_on(sl.execute("4", json!({"action": "save", "name": "grep", "description": "search", "body": "use ripgrep"}), &cancel)).unwrap();
    let skill = fs::read_to_string(dir.join("skills/grep/SKILL.md")).unwrap();
    assert!(skill.contains("confidence: 0.5") && skill.contains("use ripgrep"));
    let r = block_on(sl.execute("5", json!({"action": "reinforce", "name": "grep", "delta": 0.3}), &cancel)).unwrap();
    assert!(r.content.contains("0.80"), "reinforced confidence: {}", r.content);
    fs::remove_dir_all(&dir).ok();
}

fn git_identity_env() {
    for (k, v) in [
        ("GIT_AUTHOR_NAME", "t"), ("GIT_AUTHOR_EMAIL", "t@t"),
        ("GIT_COMMITTER_NAME", "t"), ("GIT_COMMITTER_EMAIL", "t@t"),
    ] {
        std::env::set_var(k, v);
    }
}

/// A bare remote with one commit; returns its file:// URL.
fn seed_remote(base: &Path, files: &[(&str, &str)]) -> String {
    let remote = base.join("remote.git");
    let seed = base.join("seed");
    run_git(&["-c", "init.defaultBranch=main", "init", "--bare", remote.to_str().unwrap()], base);
    fs::create_dir_all(&seed).unwrap();
    run_git(&["-c", "init.defaultBranch=main", "init"], &seed);
    fs::write(seed.join("README.md"), "hi").unwrap();
    for (p, c) in files {
        fs::write(seed.join(p), c).unwrap();
    }
    run_git(&["add", "-A"], &seed);
    run_git(&["commit", "-m", "init"], &seed);
    let url = format!("file://{}", remote.display());
    run_git(&["remote", "add", "origin", &url], &seed);
    run_git(&["push", "origin", "main"], &seed);
    url
}

#[test]
fn repo_mode_refuses_a_dir_inside_another_repo() {
    git_identity_env();
    let base = tmp_dir("repo-nonroot");
    let url = seed_remote(&base, &[]);
    // An enclosing repo with a plain (non-repo) subdirectory as the target.
    let outer = base.join("outer");
    fs::create_dir_all(outer.join("sub")).unwrap();
    run_git(&["-c", "init.defaultBranch=main", "init"], &outer);
    run_git(&["remote", "add", "origin", "https://example.invalid/keep.git"], &outer);
    fs::write(outer.join("sub").join("file.txt"), "x").unwrap();

    let res = init_local_session(RepoOptions {
        url,
        token: "tok".into(),
        dir: outer.join("sub"),
        session: None,
        trust_repo: false,
    });
    assert!(res.is_err(), "must refuse to reuse a non-root directory");
    assert_eq!(run_git(&["remote", "get-url", "origin"], &outer), "https://example.invalid/keep.git");
    fs::remove_dir_all(&base).ok();
}

#[test]
fn repo_mode_runs_repo_mcp_only_when_trusted() {
    git_identity_env();
    for trusted in [false, true] {
        let base = tmp_dir(if trusted { "repo-trust" } else { "repo-untrust" });
        let marker = base.join("marker");
        let mcp = format!("servers:\n  - name: m\n    command: \"touch {}; exit 1\"\n", marker.display());
        let url = seed_remote(&base, &[("mcp.yaml", &mcp), ("agent.yaml", "name: a\nmodel:\n  preferred: \"openai:m\"\n")]);
        let s = open_session(
            base.join("clone"),
            None,
            Some(RepoOptions { url, token: String::new(), dir: base.join("clone"), session: None, trust_repo: trusted }),
            None,
        );
        assert!(s.is_ok());
        drop(s);
        assert_eq!(marker.exists(), trusted, "repo mcp.yaml ran = {trusted}");
        fs::remove_dir_all(&base).ok();
    }
}

#[test]
fn run_agent_inherits_parent_gates() {
    std::env::set_var("OPENAI_API_KEY", "test");
    // Sub-agent: tries to write a file, then stops.
    let write = "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"w1\",\"function\":{\"name\":\"write\",\"arguments\":\"{\\\"path\\\":\\\"pwned.txt\\\",\\\"content\\\":\\\"x\\\"}\"}}]}}]}\n\ndata: [DONE]\n\n";
    let stop = "data: {\"choices\":[{\"delta\":{\"content\":\"done\"}}]}\n\ndata: [DONE]\n\n";
    let delegate = "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"function\":{\"name\":\"run_agent\",\"arguments\":\"{\\\"agent\\\":\\\"a\\\",\\\"prompt\\\":\\\"go\\\"}\"}}]}}]}\n\ndata: [DONE]\n\n";
    let found_write = |dir: &Path| {
        fn walk(p: &Path) -> bool {
            fs::read_dir(p).map(|rd| rd.flatten().any(|e| {
                let q = e.path();
                q.file_name().map(|n| n == "pwned.txt").unwrap_or(false) || (q.is_dir() && walk(&q))
            })).unwrap_or(false)
        }
        walk(&dir.join(".gitagent"))
    };
    // (parent permissions yaml, permission mode, sub-agent write expected)
    let cases = [
        ("", None, true),
        ("permissions:\n  deny: [\"write\"]\n", None, false),
        ("", Some("plan".to_string()), false),
    ];
    for (perms, mode, expect_write) in cases {
        let (base_s, sub_calls) = spawn_mock_llm(move |i| (200, if i == 0 { write.to_string() } else { stop.to_string() }));
        let (base_p, _) = spawn_mock_llm(move |i| (200, if i == 0 { delegate.to_string() } else { stop.to_string() }));
        let dir = tmp_dir("subgate");
        fs::create_dir_all(dir.join("agents/a")).unwrap();
        fs::write(dir.join("agents/a/agent.yaml"), format!("name: a\nmodel:\n  preferred: \"openai:m@{base_s}\"\n")).unwrap();
        fs::write(dir.join("agent.yaml"), format!("name: parent\nmodel:\n  preferred: \"openai:m\"\n{perms}")).unwrap();
        drain_query(QueryOptions {
            dir: dir.clone(),
            model: Some(format!("openai:m@{base_p}")),
            prompt: "delegate".into(),
            repo: None,
            permission_mode: mode.clone(),
        });
        assert_eq!(found_write(&dir), expect_write, "perms={perms:?} mode={mode:?}");
        if mode.as_deref() == Some("plan") {
            assert_eq!(sub_calls.load(Ordering::SeqCst), 0, "run_agent is mutating → blocked in plan mode");
        }
        fs::remove_dir_all(&dir).ok();
    }
}

#[cfg(unix)]
#[test]
fn cli_timeout_kills_the_whole_process_group() {
    use ira::pi::tool::AgentTool;
    let dir = tmp_dir("cligroup");
    let cli = ira::sdk::tools::Cli { cwd: dir.clone(), sandbox: None };
    let cancel = CancellationToken::new();
    let _ = block_on(async {
        tokio::time::timeout(
            Duration::from_secs(1),
            cli.execute("1", json!({"command": "sleep 300 & echo $! > pid; wait"}), &cancel),
        )
        .await
    });
    std::thread::sleep(Duration::from_millis(200));
    let pid = fs::read_to_string(dir.join("pid")).unwrap();
    let alive = Command::new("kill").args(["-0", pid.trim()]).status().unwrap().success();
    assert!(!alive, "background child must be killed with the group");
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn concurrent_prompt_is_rejected_and_turns_cancel_independently() {
    use ira::pi::agent::Agent;
    use ira::pi::event::AgentEvent;
    let (base, _) = spawn_mock_llm(|_| {
        std::thread::sleep(Duration::from_millis(400));
        (200, "data: {\"choices\":[{\"delta\":{\"content\":\"one\"}}]}\n\ndata: [DONE]\n\n".to_string())
    });
    block_on(async move {
        let agent = Agent::new("sys".into(), Vec::new(), &format!("openai:m@{base}"));
        let (mut first, first_cancel) = agent.prompt_with_cancel("a".into());
        let mut second = agent.prompt("b".into());
        let mut busy_err = None;
        while let Some(ev) = second.next().await {
            if let AgentEvent::MessageEnd(m) = ev {
                busy_err = m.error_message;
            }
        }
        assert!(busy_err.unwrap_or_default().contains("busy"));
        let mut text = String::new();
        while let Some(ev) = first.next().await {
            if let AgentEvent::MessageDelta { text: t, .. } = ev {
                text.push_str(&t);
            }
        }
        assert_eq!(text, "one");
        // The finished turn's token no longer reaches the next turn.
        let (mut third, _) = agent.prompt_with_cancel("c".into());
        first_cancel.cancel();
        let mut got = String::new();
        while let Some(ev) = third.next().await {
            if let AgentEvent::MessageDelta { text: t, .. } = ev {
                got.push_str(&t);
            }
        }
        assert_eq!(got, "one");
    });
}
