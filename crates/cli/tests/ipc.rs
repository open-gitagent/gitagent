//! `gitagent --ipc` end to end: a scripted model, the test as the client.

use std::io::{BufRead, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{json, Value};

struct Session {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: mpsc::Receiver<Value>,
}

impl Session {
    fn start(home: &std::path::Path, out: &std::path::Path) -> Session {
        let mut child = Command::new(env!("CARGO_BIN_EXE_gitagent"))
            .arg("--ipc")
            .arg("--out-dir")
            .arg(out)
            .env("HOME", home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                let v: Value = serde_json::from_str(&line)
                    .unwrap_or_else(|e| panic!("not JSON on stdout: {line:?}: {e}"));
                if tx.send(v).is_err() {
                    break;
                }
            }
        });
        Session {
            stdin: child.stdin.take(),
            child,
            lines,
        }
    }

    fn send(&mut self, v: Value) {
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{v}").unwrap();
        stdin.flush().unwrap();
    }

    fn next(&self) -> Value {
        self.lines
            .recv_timeout(Duration::from_secs(30))
            .expect("no message within 30 s")
    }

    /// Messages up to and including the first that matches.
    fn until(&self, done: impl Fn(&Value) -> bool) -> Vec<Value> {
        let mut seen = Vec::new();
        loop {
            let m = self.next();
            let stop = done(&m);
            seen.push(m);
            if stop {
                return seen;
            }
        }
    }

    /// Answers callback requests with `answer` until a message matches.
    fn serve(
        &mut self,
        answer: impl Fn(&Value) -> Value,
        done: impl Fn(&Value) -> bool,
    ) -> Vec<Value> {
        let mut seen = Vec::new();
        loop {
            let m = self.next();
            if m["type"] == "callback_request" {
                let result = answer(&m);
                self.send(json!({"type": "callback_response", "id": m["id"], "result": result}));
            }
            let stop = done(&m);
            seen.push(m);
            if stop {
                return seen;
            }
        }
    }

    fn finish(mut self) -> (i32, Vec<Value>) {
        drop(self.stdin.take());
        let mut rest = Vec::new();
        while let Ok(m) = self.lines.recv_timeout(Duration::from_secs(30)) {
            rest.push(m);
        }
        let code = self.child.wait().unwrap().code().unwrap_or(-1);
        (code, rest)
    }
}

fn is<'a>(kind: &'a str, subtype: &'a str) -> impl Fn(&Value) -> bool + 'a {
    move |m: &Value| m["type"] == kind && (subtype.is_empty() || m["subtype"] == subtype)
}

fn agent(dir: &std::path::Path, extra: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join("agent.yaml"),
        format!("spec_version: \"0.1.0\"\nname: lyra\nversion: 1.0.0\ndescription: A test agent.\n{extra}"),
    )
    .unwrap();
    std::fs::write(dir.join("SOUL.md"), "I am Lyra.\n").unwrap();
}

fn script(dir: &std::path::Path, name: &str, text: &str) -> String {
    let path = dir.join(name);
    std::fs::write(&path, text).unwrap();
    format!("scripted:{}", path.display())
}

#[test]
fn turns_hooks_approvals_client_tools_and_resume() {
    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path().join("work");
    agent(
        &work,
        "compliance:\n  risk_tier: high\n  supervision: {human_in_the_loop: always}\n",
    );
    let model = script(
        tmp.path(),
        "s.yaml",
        "- tool_calls: [{name: exec, arguments: {cmd: \"rm -rf build\"}}]\n\
         - tool_calls: [{name: exec, arguments: {cmd: \"echo ok > ok.txt\"}}]\n\
         - text: \"Cleaned up.\"\n\
         - tool_calls: [{name: lookup_order, arguments: {id: \"42\"}}]\n\
         - text: \"Order 42 has shipped.\"\n",
    );
    let mut s = Session::start(tmp.path(), &tmp.path().join("out"));
    s.send(json!({
        "type": "initialize", "protocol": 1,
        "options": {"model": model, "dir": work, "sessionId": "t1", "resume": true, "toolNames": "v1"},
        "callbacks": ["preToolUse", "approval"],
        "tools": [{"name": "lookup_order", "description": "Looks up an order.", "inputSchema": {"properties": {"id": {"type": "string"}}}}]
    }));
    let ready = s.next();
    assert_eq!(
        (ready["type"].as_str(), ready["subtype"].as_str()),
        (Some("system"), Some("ready"))
    );
    let meta = &ready["metadata"];
    assert_eq!(meta["protocol"], 1);
    assert_eq!(meta["sessionId"], "t1");
    assert_eq!(meta["agent"]["name"], "lyra");
    let tools: Vec<&str> = meta["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t.as_str().unwrap())
        .collect();
    assert!(
        tools.contains(&"cli")
            && tools.contains(&"lookup_order")
            && !tools.contains(&"task_complete"),
        "{tools:?}"
    );
    assert!(is("system", "session_start")(&s.next()));

    s.send(json!({"type": "user", "content": "clean up"}));
    let answer = |m: &Value| match m["kind"].as_str() {
        Some("hook") => {
            assert_eq!(m["name"], "preToolUse");
            let tool = m["input"]["toolName"].as_str().unwrap();
            assert!(matches!(tool, "cli" | "lookup_order"), "{tool}");
            if m["input"]["args"]["command"]
                .as_str()
                .unwrap_or("")
                .contains("rm -rf")
            {
                json!({"action": "block", "reason": "no rm here"})
            } else {
                json!({"action": "allow"})
            }
        }
        Some("approval") => {
            assert!(m["input"]["reason"]
                .as_str()
                .unwrap()
                .contains("human_in_the_loop: always"));
            json!({"approved": true})
        }
        Some("tool") => {
            assert_eq!(m["input"], json!({"id": "42"}));
            json!({"content": "shipped on Monday"})
        }
        other => panic!("unexpected callback {other:?}"),
    };
    let turn = s.serve(answer, is("system", "turn_end"));
    let kinds: Vec<String> = turn
        .iter()
        .map(|m| {
            format!(
                "{}{}",
                m["type"].as_str().unwrap(),
                m["kind"]
                    .as_str()
                    .map_or(String::new(), |k| format!(":{k}"))
            )
        })
        .collect();
    assert_eq!(
        kinds,
        vec![
            "user",
            "assistant",
            "tool_use",
            "callback_request:hook",
            "tool_result",
            "assistant",
            "tool_use",
            "callback_request:hook",
            "callback_request:approval",
            "tool_result",
            "delta",
            "assistant",
            "system"
        ]
    );
    let results: Vec<&Value> = turn.iter().filter(|m| m["type"] == "tool_result").collect();
    assert_eq!(results[0]["isError"], true);
    assert!(results[0]["content"]
        .as_str()
        .unwrap()
        .contains("no rm here"));
    assert_eq!(results[1]["toolName"], "cli");
    assert!(work.join("ok.txt").exists());
    let end = turn.last().unwrap();
    assert_eq!(end["metadata"]["outcome"], "completed");
    assert_eq!(end["metadata"]["result"], "Cleaned up.");

    s.send(json!({"type": "user", "content": "where is order 42?"}));
    let turn = s.serve(answer, is("system", "turn_end"));
    let result = turn.iter().find(|m| m["type"] == "tool_result").unwrap();
    assert_eq!(result["content"], "shipped on Monday");
    let first_input = turn.iter().find(|m| m["type"] == "assistant").unwrap()["usage"]
        ["inputTokens"]
        .as_u64()
        .unwrap();
    s.send(json!({"type": "end"}));
    let (code, rest) = s.finish();
    assert_eq!(code, 0);
    let end = rest.last().unwrap();
    assert!(is("system", "session_end")(end));
    assert_eq!(end["metadata"]["costs"]["requests"], 5);
    assert!(work.join(".gitagent/sessions/t1/messages.json").is_file());

    // A new process continues the stored conversation: its first request is
    // larger than one without the history.
    let model = script(tmp.path(), "r.yaml", "- text: \"Still here.\"\n");
    let first_turn_tokens = |resume: bool, id: &str| {
        let mut s = Session::start(tmp.path(), &tmp.path().join(format!("out-{id}")));
        s.send(json!({
            "type": "initialize", "protocol": 1,
            "options": {"model": model, "dir": work, "sessionId": id, "resume": resume}
        }));
        s.send(json!({"type": "user", "content": "hello again"}));
        let turn = s.until(is("system", "turn_end"));
        s.send(json!({"type": "end"}));
        s.finish();
        turn.iter().find(|m| m["type"] == "assistant").unwrap()["usage"]["inputTokens"]
            .as_u64()
            .unwrap()
    };
    let resumed = first_turn_tokens(true, "t1");
    let fresh = first_turn_tokens(false, "t2");
    assert!(
        resumed > fresh + 100,
        "resumed {resumed}, fresh {fresh}, earlier {first_input}"
    );
}

#[test]
fn steering_and_abort() {
    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let model = script(
        tmp.path(),
        "s.yaml",
        "- tool_calls: [{name: exec, arguments: {cmd: \"sleep 2\", yield_ms: 4000}}]\n\
         - text: \"Done, and hi.\"\n\
         - tool_calls: [{name: exec, arguments: {cmd: \"sleep 20\", yield_ms: 3000}}]\n\
         - tool_calls: [{name: exec, arguments: {cmd: \"echo never > never.txt\"}}]\n\
         - text: \"unreachable\"\n",
    );
    let out = tmp.path().join("out");
    let mut s = Session::start(tmp.path(), &out);
    s.send(json!({"type": "initialize", "protocol": 1, "options": {"model": model, "dir": work, "sessionId": "st"}}));
    s.until(is("system", "session_start"));

    s.send(json!({"type": "user", "content": "wait a bit"}));
    s.until(|m| m["type"] == "tool_use");
    s.send(json!({"type": "steer", "content": "also say hi"}));
    let turn = s.until(is("system", "turn_end"));
    assert_eq!(turn.last().unwrap()["metadata"]["result"], "Done, and hi.");
    let t = std::fs::read_to_string(out.join("st/turn-1/trajectory.json")).unwrap();
    assert!(
        t.contains("Message from the user while you were working") && t.contains("also say hi")
    );

    s.send(json!({"type": "user", "content": "wait longer"}));
    s.until(|m| m["type"] == "tool_use");
    s.send(json!({"type": "abort"}));
    let turn = s.until(is("system", "turn_end"));
    assert_eq!(
        turn.last().unwrap()["metadata"]["outcome"],
        "killed: aborted by the client"
    );
    assert!(!work.join("never.txt").exists());
    s.send(json!({"type": "end"}));
    let (code, rest) = s.finish();
    assert_eq!(code, 0);
    assert!(is("system", "session_end")(rest.last().unwrap()));
}

#[test]
fn startup_problems_and_blocking_hooks() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = Session::start(tmp.path(), &tmp.path().join("out"));
    s.send(json!({"type": "initialize", "protocol": 9, "options": {}}));
    let (code, rest) = s.finish();
    assert_eq!(code, 2);

    // A malformed client tool is refused, not a crash.
    let mut s = Session::start(tmp.path(), &tmp.path().join("out0"));
    s.send(json!({"type": "initialize", "protocol": 1, "options": {},
                  "tools": [{"name": "t", "inputSchema": "string"}]}));
    let (code, rest0) = s.finish();
    assert_eq!(code, 2);
    assert!(rest0[0]["content"]
        .as_str()
        .unwrap()
        .contains("`inputSchema` must be an object"));
    assert!(rest[0]["content"]
        .as_str()
        .unwrap()
        .contains("protocol 9 is not supported"));
    assert!(is("system", "session_end")(&rest[1]));

    let work = tmp.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let model = script(tmp.path(), "s.yaml", "- text: \"hi\"\n");
    let mut s = Session::start(tmp.path(), &tmp.path().join("out2"));
    s.send(json!({
        "type": "initialize", "protocol": 1,
        "options": {"model": model, "dir": work},
        "callbacks": ["preQuery"]
    }));
    s.until(is("system", "session_start"));
    s.send(json!({"type": "user", "content": "anything"}));
    let turn = s.serve(
        |_| json!({"action": "block", "reason": "closed today"}),
        is("system", "turn_end"),
    );
    assert!(turn.iter().any(|m| is("system", "hook_blocked")(m)
        && m["content"] == "Query blocked by hook: closed today"));
    assert_eq!(turn.last().unwrap()["metadata"]["outcome"], "hook_blocked");
    let (code, _) = s.finish();
    assert_eq!(code, 0);
}

/// 1.x's `repo` option: the session works in a clone on a session branch
/// and the changes are committed and pushed when it ends; a second session
/// with `session` set to that branch reuses the clone and resumes it.
#[test]
fn the_repo_option_clones_commits_and_resumes() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let git = |dir: &std::path::Path, args: &[&str]| {
        let out = std::process::Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    git(root, &["init", "-q", "--bare", "-b", "main", "origin.git"]);
    git(root, &["init", "-q", "-b", "main", "seed"]);
    let seed = root.join("seed");
    agent(&seed, "");
    git(&seed, &["add", "."]);
    git(
        &seed,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "-m",
            "init",
        ],
    );
    let url = root.join("origin.git").to_string_lossy().into_owned();
    git(&seed, &["push", "-q", &url, "main"]);

    let model = script(
        root,
        "s.yaml",
        "- tool_calls: [{name: write_file, arguments: {path: fix.txt, content: \"fixed\\n\"}}]\n\
         - text: \"Added fix.txt.\"\n",
    );
    let clone = root.join("clone");
    let mut s = Session::start(root, &root.join("out"));
    s.send(json!({
        "type": "initialize", "protocol": 1,
        "options": {"model": model, "repo": {"url": url, "dir": clone, "session": "gitagent/session-ab12"}}
    }));
    s.until(is("system", "session_start"));
    s.send(json!({"type": "user", "content": "fix it"}));
    s.until(is("system", "turn_end"));
    s.send(json!({"type": "end"}));
    let (code, rest) = s.finish();
    assert_eq!(code, 0);
    let end = rest
        .iter()
        .find(|m| is("system", "session_end")(m))
        .unwrap();
    assert_eq!(end["metadata"]["branch"], "gitagent/session-ab12");
    assert_eq!(end["metadata"]["pushed"], true);
    assert!(end["metadata"]["commit"].is_string(), "{end}");
    assert!(clone.join("fix.txt").is_file());
    let remote = git(root, &["ls-remote", "--heads", &url]);
    assert!(
        remote.contains("refs/heads/gitagent/session-ab12"),
        "{remote}"
    );
    assert_eq!(
        git(&clone, &["log", "-1", "--format=%s"]).trim(),
        "gitagent: auto-commit (gitagent/session-ab12)"
    );

    // The same clone and branch again, with nothing to commit.
    let model = script(root, "t.yaml", "- text: \"Nothing to do.\"\n");
    let mut s = Session::start(root, &root.join("out2"));
    s.send(json!({
        "type": "initialize", "protocol": 1,
        "options": {"model": model, "repo": {"url": url, "dir": clone, "session": "gitagent/session-ab12", "autoCommit": false}}
    }));
    s.until(is("system", "session_start"));
    s.send(json!({"type": "user", "content": "anything"}));
    s.until(is("system", "turn_end"));
    s.send(json!({"type": "end"}));
    let (code, rest) = s.finish();
    assert_eq!(code, 0);
    let end = rest
        .iter()
        .find(|m| is("system", "session_end")(m))
        .unwrap();
    assert_eq!(end["metadata"]["branch"], "gitagent/session-ab12");
    assert!(end["metadata"].get("pushed").is_none(), "{end}");
}
