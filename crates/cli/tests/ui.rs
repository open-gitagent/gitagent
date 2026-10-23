//! `gitagent ui`: the local web page's server, driven over plain HTTP.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

struct Server {
    child: Child,
    port: u16,
}

impl Server {
    fn start(agent: &std::path::Path, model: &str, out: &std::path::Path) -> Server {
        let mut child = Command::new(env!("CARGO_BIN_EXE_gitagent"))
            .args(["ui", "--port", "0", "--no-open", "-d"])
            .arg(agent)
            .arg("-m")
            .arg(model)
            .arg("--out-dir")
            .arg(out)
            .env("HOME", agent.parent().unwrap())
            .env("TZ", "UTC")
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let mut first = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut first)
            .unwrap();
        let port: u16 = first
            .trim()
            .rsplit(':')
            .next()
            .unwrap()
            .trim_end_matches('/')
            .parse()
            .unwrap_or_else(|_| panic!("no url in {first:?}"));
        Server { child, port }
    }

    fn request(&self, method: &str, path: &str, body: Option<&Value>) -> (u16, String) {
        let mut s = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        let body = body.map(|b| b.to_string()).unwrap_or_default();
        write!(
            s,
            "{method} {path} HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut text = String::new();
        s.read_to_string(&mut text).unwrap();
        let (head, body) = text.split_once("\r\n\r\n").unwrap();
        let code: u16 = head.split(' ').nth(1).unwrap().parse().unwrap();
        (code, body.to_string())
    }

    fn get(&self, path: &str) -> Value {
        let (code, body) = self.request("GET", path, None);
        assert_eq!(code, 200, "GET {path}: {body}");
        serde_json::from_str(&body).unwrap_or_else(|_| panic!("GET {path}: not JSON: {body}"))
    }

    fn post(&self, path: &str, body: Value) -> Value {
        let (code, text) = self.request("POST", path, Some(&body));
        assert_eq!(code, 200, "POST {path}: {text}");
        serde_json::from_str(&text).unwrap()
    }

    /// Opens the event stream and returns events until `done` matches.
    fn events_until(&self, done: impl Fn(&Value) -> bool, act: impl FnOnce(&Server)) -> Vec<Value> {
        let mut s = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
        write!(s, "GET /api/events HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let mut r = BufReader::new(s);
        let mut line = String::new();
        loop {
            line.clear();
            r.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
        }
        act(self);
        let mut out = Vec::new();
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(60) {
            line.clear();
            if r.read_line(&mut line).unwrap() == 0 {
                break;
            }
            if let Some(data) = line.strip_prefix("data: ") {
                let v: Value = serde_json::from_str(data.trim()).unwrap();
                let finished = done(&v);
                out.push(v);
                if finished {
                    return out;
                }
            }
        }
        panic!("the event stream ended without the expected event: {out:#?}");
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // Ctrl-C, so the server ends its sessions before the directory goes.
        let _ = Command::new("kill")
            .args(["-INT", &self.child.id().to_string()])
            .status();
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(10) {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn agent(root: &std::path::Path, extra: &str) -> std::path::PathBuf {
    let dir = root.join("agent");
    std::fs::create_dir_all(dir.join("schedules")).unwrap();
    std::fs::write(
        dir.join("agent.yaml"),
        format!("spec_version: \"0.1.0\"\nname: lyra\nversion: 1.0.0\ndescription: A test agent.\n{extra}"),
    )
    .unwrap();
    std::fs::write(dir.join("SOUL.md"), "I am Lyra.\n").unwrap();
    std::fs::write(dir.join("RULES.md"), "# Rules\n\n- Be brief.\n").unwrap();
    std::fs::write(
        dir.join("schedules/tidy.yaml"),
        "id: tidy\ndescription: Tidy up\ncron: \"0 2 * * *\"\nprompt: Tidy the notes.\n",
    )
    .unwrap();
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .current_dir(&dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["add", "-A"]);
    git(&[
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t",
        "commit",
        "-q",
        "-m",
        "Start the agent",
    ]);
    dir
}

fn script(root: &std::path::Path, text: &str) -> String {
    let p = root.join("s.yaml");
    std::fs::write(&p, text).unwrap();
    format!("scripted:{}", p.display())
}

#[test]
fn the_page_and_its_api_serve_an_agent() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = agent(tmp.path(), "");
    let model = script(
        tmp.path(),
        "- tool_calls: [{name: write_file, arguments: {path: notes.txt, content: \"hello\\n\"}}]\n\
         - text: \"Wrote **notes.txt**.\"\n\
         - text: \"Second turn.\"\n",
    );
    let srv = Server::start(&dir, &model, &tmp.path().join("out"));

    // The page itself.
    let (code, page) = srv.request("GET", "/", None);
    assert_eq!(code, 200);
    assert!(
        page.starts_with("<!doctype html>") && page.contains("<title>gitagent</title>"),
        "{}",
        &page[..200]
    );
    let (code, _) = srv.request("GET", "/nope", None);
    assert_eq!(code, 404);

    // State before anything ran.
    let st = srv.get("/api/state");
    assert_eq!(st["agent"]["name"], "lyra");
    assert_eq!(st["agent"]["branch"], "main");
    assert!(
        st["tools"].as_array().unwrap().iter().any(|t| t == "cli"),
        "{}",
        st["tools"]
    );
    assert_eq!(st["session"]["id"], "");
    assert_eq!(st["schedules"][0]["id"], "tidy");
    assert_eq!(st["changes"]["commits"][0]["subject"], "Start the agent");

    // A turn: the protocol's messages arrive on the event stream.
    let events = srv.events_until(
        |v| v["type"] == "system" && v["subtype"] == "turn_end",
        |s| {
            s.post("/api/send", json!({"text": "write notes"}));
        },
    );
    let kinds: Vec<String> = events
        .iter()
        .map(|e| {
            format!(
                "{}{}",
                e["type"].as_str().unwrap_or(""),
                e["subtype"]
                    .as_str()
                    .map(|s| format!("/{s}"))
                    .unwrap_or_default()
            )
        })
        .collect();
    assert!(
        kinds.contains(&"system/session_start".into())
            && kinds.contains(&"user".into())
            && kinds.contains(&"tool_use".into())
            && kinds.contains(&"assistant".into()),
        "{kinds:?}"
    );
    let tool = events.iter().find(|e| e["type"] == "tool_use").unwrap();
    assert_eq!(tool["toolName"], "write");
    assert_eq!(
        std::fs::read_to_string(dir.join("notes.txt")).unwrap(),
        "hello\n"
    );
    assert!(events.iter().any(|e| e["type"] == "changes"));

    // The state replays the session's messages and names it.
    let st = srv.get("/api/state");
    assert!(st["session"]["id"]
        .as_str()
        .unwrap()
        .starts_with("ui-chat-"));
    assert!(
        st["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["type"] == "assistant" && m["content"] == "Wrote **notes.txt**."),
        "{}",
        st["messages"]
    );
    assert_eq!(st["changes"]["uncommitted"][0]["path"], "notes.txt");

    // Files: list, read, save; the change shows in the working diff.
    let files = srv.get("/api/files?path=");
    let names: Vec<&str> = files["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert!(
        names.contains(&"SOUL.md") && names.contains(&"schedules") && !names.contains(&".git"),
        "{names:?}"
    );
    assert_eq!(srv.get("/api/file?path=SOUL.md")["content"], "I am Lyra.\n");
    assert_eq!(srv.get("/api/file?path=nope.md")["kind"], "missing");
    let (code, msg) = srv.request("GET", "/api/file?path=../s.yaml", None);
    assert_eq!(code, 400, "{msg}");
    srv.post(
        "/api/file",
        json!({"path": "SOUL.md", "content": "I am Lyra, tidier.\n"}),
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("SOUL.md")).unwrap(),
        "I am Lyra, tidier.\n"
    );
    let diff = srv.get("/api/change?sha=working")["text"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        diff.contains("-I am Lyra.")
            && diff.contains("+I am Lyra, tidier.")
            && diff.contains("+++ b/notes.txt"),
        "{diff}"
    );

    // The prompt the model sees, split into sections.
    let prompt = srv.get("/api/prompt");
    let titles: Vec<&str> = prompt["sections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["title"].as_str().unwrap())
        .collect();
    assert!(
        titles[0].contains("lyra") && titles.iter().any(|t| t.contains("rules")),
        "{titles:?}"
    );
    assert_eq!(prompt["sections"][0]["source"], "SOUL.md");

    // Commit history and revert.
    let sha = srv.get("/api/changes")["commits"][0]["sha"]
        .as_str()
        .unwrap()
        .to_string();
    let show = srv.get(&format!("/api/change?sha={sha}"))["text"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        show.contains("Start the agent") && show.contains("+I am Lyra."),
        "{show}"
    );
    let out = Command::new("git")
        .current_dir(&dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qam",
            "tidier",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let head = srv.get("/api/changes")["commits"][0]["sha"]
        .as_str()
        .unwrap()
        .to_string();
    let r = srv.post("/api/revert", json!({"sha": head}));
    assert!(r["ok"] == true);
    assert_eq!(
        std::fs::read_to_string(dir.join("SOUL.md")).unwrap(),
        "I am Lyra.\n"
    );
    assert!(srv.get("/api/changes")["commits"][0]["subject"]
        .as_str()
        .unwrap()
        .starts_with("Revert"));

    // Schedules: list, disable, enable.
    assert_eq!(srv.get("/api/schedules")["schedules"][0]["enabled"], true);
    srv.post("/api/schedule", json!({"id": "tidy", "action": "disable"}));
    assert_eq!(srv.get("/api/schedules")["schedules"][0]["enabled"], false);
    srv.post("/api/schedule", json!({"id": "tidy", "action": "enable"}));
    let (code, _) = srv.request(
        "POST",
        "/api/schedule",
        Some(&json!({"id": "nope", "action": "run"})),
    );
    assert_eq!(code, 404);

    // A new session ends the old one, which is then listed with its title.
    let r = srv.post("/api/new", json!({}));
    assert!(r["sessionId"].as_str().unwrap().starts_with("ui-chat-"));
    let deadline = Instant::now() + Duration::from_secs(15);
    let sessions = loop {
        let s = srv.get("/api/sessions")["sessions"].clone();
        if !s.as_array().unwrap().is_empty() || Instant::now() > deadline {
            break s;
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    assert_eq!(sessions[0]["title"], "write notes", "{sessions}");
    let detail = srv.get(&format!(
        "/api/session?id={}",
        sessions[0]["id"].as_str().unwrap()
    ));
    assert!(detail["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["type"] == "tool_use"));
}

/// Compliance approvals: the request reaches the page, the answer reaches
/// the agent, and the inbox keeps the history.
#[test]
fn approvals_flow_through_the_inbox() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = agent(
        tmp.path(),
        "compliance:\n  risk_tier: high\n  supervision: {human_in_the_loop: always}\n",
    );
    let model = script(
        tmp.path(),
        "- tool_calls: [{name: exec, arguments: {cmd: \"echo one > one.txt\"}}]\n- text: \"done one\"\n",
    );
    let srv = Server::start(&dir, &model, &tmp.path().join("out"));
    let events = srv.events_until(
        |v| v["type"] == "approval" && v["action"] == "request",
        |s| {
            s.post("/api/send", json!({"text": "do it"}));
        },
    );
    let req = events.last().unwrap();
    assert_eq!(req["toolName"], "cli");
    assert_eq!(req["args"]["command"], "echo one > one.txt");
    let id = req["id"].as_str().unwrap().to_string();
    let st = srv.get("/api/state");
    assert_eq!(st["approvals"][0]["id"], id);

    let events = srv.events_until(
        |v| v["type"] == "system" && v["subtype"] == "turn_end",
        |s| {
            s.post(
                "/api/approve",
                json!({"id": id, "approved": true, "reason": "fine"}),
            );
        },
    );
    assert!(events
        .iter()
        .any(|e| e["type"] == "approval" && e["action"] == "answered" && e["approved"] == true));
    assert_eq!(
        std::fs::read_to_string(dir.join("one.txt")).unwrap(),
        "one\n"
    );
    let st = srv.get("/api/state");
    assert!(st["approvals"].as_array().unwrap().is_empty());
    assert_eq!(st["history"][0]["approved"], true);
    let (code, msg) = srv.request(
        "POST",
        "/api/approve",
        Some(&json!({"id": "c-99", "approved": false})),
    );
    assert_eq!(code, 500, "{msg}");
    assert!(msg.contains("no approval"));
}

/// A schedule run from the page runs as its own session and is recorded.
#[test]
fn a_schedule_run_from_the_page_is_recorded() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = agent(tmp.path(), "");
    let model = script(tmp.path(), "- text: \"Tidied.\"\n");
    let srv = Server::start(&dir, &model, &tmp.path().join("out"));
    let events = srv.events_until(
        |v| v["type"] == "schedule" && v["action"] == "result",
        |s| {
            s.post("/api/schedule", json!({"id": "tidy", "action": "run"}));
        },
    );
    let result = events.last().unwrap();
    assert_eq!(result["success"], true, "{result}");
    assert_eq!(result["result"], "Tidied.");
    assert!(events
        .iter()
        .any(|e| e["type"] == "assistant"
            && e["sessionId"].as_str().unwrap().starts_with("ui-tidy-")));
    let file = std::fs::read_to_string(dir.join("schedules/tidy.yaml")).unwrap();
    assert!(file.contains("lastResult: success"), "{file}");
    assert!(dir.join(".gitagent/schedule-logs/tidy.jsonl").is_file());
    assert_eq!(
        srv.get("/api/schedules")["schedules"][0]["lastResult"],
        "success"
    );
}
