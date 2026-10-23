//! `gitagent ui [-d dir] [-m model] [-e env] [--port N] [--out-dir] [--no-open]`:
//! a local web page for one agent directory.
//!
//! The page is a client of the IPC protocol like the SDKs: this server
//! starts `gitagent --ipc` for each conversation, relays its messages to the
//! browser as server-sent events, and turns the browser's actions into
//! protocol messages. Compliance approvals become an inbox; the agent's
//! files and git history are read and changed in place; schedules fire
//! while the server runs. Everything listens on 127.0.0.1 only.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::{ExitCode, Stdio};
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use gitagent_loader::schedules::{self, Schedule};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{broadcast, Mutex};

const PAGE: &str = include_str!("ui/index.html");

/// Sees every message of a session as it arrives.
type Watcher = Arc<dyn Fn(&Value) + Send + Sync>;

#[derive(Debug, Parser)]
#[command(name = "gitagent ui", about = "A local web page for an agent.")]
struct UiCli {
    /// The agent directory (default: the current directory).
    #[arg(short = 'd', long)]
    dir: Option<PathBuf>,
    /// The model, as provider:model or a model id.
    #[arg(short = 'm', long)]
    model: Option<String>,
    /// The environment (config/<env>.yaml).
    #[arg(short = 'e', long)]
    env: Option<String>,
    /// The port on 127.0.0.1 (0 picks a free one).
    #[arg(long, default_value_t = 3333)]
    port: u16,
    /// Where run records go (default: /tmp/gitagent/ui).
    #[arg(long)]
    out_dir: Option<PathBuf>,
    /// Do not open the browser.
    #[arg(long)]
    no_open: bool,
}

pub(crate) async fn run(args: &[std::ffi::OsString]) -> ExitCode {
    let cli = match UiCli::try_parse_from(
        std::iter::once("gitagent ui".into()).chain(args.iter().cloned()),
    ) {
        Ok(c) => c,
        Err(e) => {
            let _ = e.print();
            return ExitCode::from(if e.use_stderr() { 2 } else { 0 });
        }
    };
    let dir = cli.dir.clone().unwrap_or_else(|| PathBuf::from("."));
    let dir = std::path::absolute(&dir).unwrap_or(dir);
    if !dir.join("agent.yaml").is_file() {
        eprintln!(
            "gitagent ui: {} has no agent.yaml; `gitagent init {}` starts one",
            dir.display(),
            dir.display()
        );
        return ExitCode::from(2);
    }
    crate::load_env_files(Some(&dir));
    let listener = match TcpListener::bind(("127.0.0.1", cli.port)).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("gitagent ui: cannot listen on 127.0.0.1:{}: {e}", cli.port);
            return ExitCode::from(2);
        }
    };
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(cli.port);
    let ui = Arc::new(Ui::new(
        dir,
        cli.model.clone(),
        cli.env.clone(),
        cli.out_dir
            .clone()
            .unwrap_or_else(|| PathBuf::from("/tmp/gitagent/ui")),
    ));
    let url = format!("http://127.0.0.1:{port}/");
    println!("gitagent ui: {url}");
    eprintln!(
        "gitagent ui: serving {} at {url} (Ctrl-C stops; schedules fire while this runs)",
        ui.dir.display()
    );
    if !cli.no_open {
        open_browser(&url);
    }
    tokio::spawn(scheduler(ui.clone()));
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    let ui = ui.clone();
                    tokio::spawn(async move {
                        if let Err(e) = serve(stream, ui).await {
                            if !e.to_string().contains("Broken pipe") {
                                eprintln!("gitagent ui: {e}");
                            }
                        }
                    });
                }
                Err(e) => eprintln!("gitagent ui: accept: {e}"),
            },
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    ui.shutdown().await;
    eprintln!("gitagent ui: stopped");
    ExitCode::SUCCESS
}

fn open_browser(url: &str) {
    for cmd in ["xdg-open", "open"] {
        if std::process::Command::new(cmd)
            .arg(url)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .is_ok()
        {
            return;
        }
    }
}

// ---------------------------------------------------------------- state

struct Ui {
    dir: PathBuf,
    model: Option<String>,
    env: Option<String>,
    out_dir: PathBuf,
    events: broadcast::Sender<Value>,
    chat: Mutex<Option<Session>>,
    approvals: Mutex<Approvals>,
    /// One scheduled job at a time.
    schedule_lock: Mutex<()>,
    /// Where approval answers for running scheduled jobs go.
    job_writers: Mutex<HashMap<String, tokio::sync::mpsc::Sender<Value>>>,
}

#[derive(Default)]
struct Approvals {
    pending: Vec<Value>,
    history: Vec<Value>,
    /// Which session answers which callback id.
    owners: HashMap<String, String>,
}

/// One `gitagent --ipc` child and what it has said so far.
struct Session {
    id: String,
    stdin: ChildStdin,
    child: Child,
    messages: Arc<Mutex<Vec<Value>>>,
    running: Arc<std::sync::atomic::AtomicBool>,
    ended: Arc<std::sync::atomic::AtomicBool>,
}

impl Ui {
    fn new(dir: PathBuf, model: Option<String>, env: Option<String>, out_dir: PathBuf) -> Ui {
        let (events, _) = broadcast::channel(512);
        Ui {
            dir,
            model,
            env,
            out_dir,
            events,
            chat: Mutex::new(None),
            approvals: Mutex::new(Approvals::default()),
            schedule_lock: Mutex::new(()),
            job_writers: Mutex::new(HashMap::new()),
        }
    }

    fn emit(&self, v: Value) {
        let _ = self.events.send(v);
    }

    /// Starts an IPC session; `kind` names it (`chat`, or a schedule id).
    async fn start_session(
        self: &Arc<Self>,
        kind: &str,
        session_id: Option<String>,
        on_message: Option<Watcher>,
    ) -> anyhow::Result<Session> {
        let id = session_id.unwrap_or_else(|| {
            format!(
                "ui-{}-{}",
                kind,
                schedules::utc_now_rfc3339()
                    .replace([':', '-'], "")
                    .trim_end_matches('Z')
            )
        });
        let exe = std::env::current_exe()?;
        let out = self.out_dir.join(&id);
        let mut child = Command::new(exe)
            .arg("--ipc")
            .arg("--out-dir")
            .arg(&out)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()?;
        let mut stdin = child.stdin.take().expect("piped");
        let stdout = child.stdout.take().expect("piped");
        let mut options = json!({
            "dir": self.dir, "sessionId": id, "resume": true, "toolNames": "v1", "conversation": true,
        });
        if let Some(m) = &self.model {
            options["model"] = json!(m);
        }
        if let Some(e) = &self.env {
            options["env"] = json!(e);
        }
        let init = json!({"type": "initialize", "protocol": 1, "options": options, "callbacks": ["approval"]});
        stdin.write_all(format!("{init}\n").as_bytes()).await?;
        let messages = Arc::new(Mutex::new(Vec::new()));
        let running = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let ended = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (m2, r2, e2, ui, sid) = (
            messages.clone(),
            running.clone(),
            ended.clone(),
            self.clone(),
            id.clone(),
        );
        let is_chat = kind == "chat";
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            let log_path = ui
                .dir
                .join(".gitagent/sessions")
                .join(&sid)
                .join("ui.jsonl");
            let _ = std::fs::create_dir_all(log_path.parent().unwrap_or(Path::new(".")));
            let started = schedules::utc_now_rfc3339();
            let mut title = String::new();
            let mut costs = Value::Null;
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(mut v) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                v["sessionId"] = json!(sid);
                match v["type"].as_str() {
                    Some("callback_request") => {
                        ui.on_callback(&sid, &v).await;
                        continue;
                    }
                    Some("user") => {
                        r2.store(true, std::sync::atomic::Ordering::Relaxed);
                        if title.is_empty() {
                            title = v["content"]
                                .as_str()
                                .unwrap_or_default()
                                .chars()
                                .take(80)
                                .collect();
                        }
                    }
                    Some("system") => match v["subtype"].as_str() {
                        Some("turn_end") => r2.store(false, std::sync::atomic::Ordering::Relaxed),
                        Some("session_end") => {
                            e2.store(true, std::sync::atomic::Ordering::Relaxed);
                            r2.store(false, std::sync::atomic::Ordering::Relaxed);
                            costs = v["metadata"]["costs"].clone();
                        }
                        _ => {}
                    },
                    _ => {}
                }
                if let Some(f) = &on_message {
                    f(&v);
                }
                if is_chat {
                    m2.lock().await.push(v.clone());
                    if let Ok(mut f) = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&log_path)
                    {
                        use std::io::Write as _;
                        let _ = writeln!(f, "{v}");
                    }
                }
                let is_tool_result = v["type"] == "tool_result";
                ui.emit(v);
                if is_tool_result {
                    ui.emit(json!({"type": "changes"}));
                }
            }
            e2.store(true, std::sync::atomic::Ordering::Relaxed);
            r2.store(false, std::sync::atomic::Ordering::Relaxed);
            if is_chat {
                let meta = json!({"id": sid, "title": title, "started": started, "ended": schedules::utc_now_rfc3339(), "costs": costs});
                let _ = std::fs::write(
                    log_path.with_file_name("ui.json"),
                    serde_json::to_vec_pretty(&meta).unwrap_or_default(),
                );
                ui.emit(json!({"type": "changes"}));
            }
        });
        Ok(Session {
            id,
            stdin,
            child,
            messages,
            running,
            ended,
        })
    }

    /// A `callback_request` from a session: approvals go to the inbox; the
    /// protocol's other callbacks are not registered, so none arrive.
    async fn on_callback(&self, session: &str, v: &Value) {
        if v["kind"] != "approval" {
            return;
        }
        let id = v["id"].as_str().unwrap_or_default().to_string();
        let input = &v["input"];
        let req = json!({
            "type": "approval", "action": "request", "id": id, "sessionId": session,
            "toolName": input["toolName"], "args": input["args"], "reason": input["reason"],
            "ts": schedules::utc_now_rfc3339(),
        });
        let mut a = self.approvals.lock().await;
        a.owners.insert(id, session.to_string());
        a.pending.push(req.clone());
        drop(a);
        self.emit(req);
    }

    async fn answer_approval(&self, id: &str, approved: bool, reason: &str) -> anyhow::Result<()> {
        let mut a = self.approvals.lock().await;
        let Some(owner) = a.owners.remove(id) else {
            anyhow::bail!("no approval {id:?} is waiting");
        };
        let idx = a.pending.iter().position(|p| p["id"] == id);
        let mut req = idx
            .map(|i| a.pending.remove(i))
            .unwrap_or_else(|| json!({"id": id}));
        req["action"] = json!("answered");
        req["approved"] = json!(approved);
        req["answer"] = json!(reason);
        req["answeredAt"] = json!(schedules::utc_now_rfc3339());
        a.history.insert(0, req.clone());
        a.history.truncate(200);
        drop(a);
        let line = json!({"type": "callback_response", "id": id, "result": {"approved": approved, "reason": reason}});
        let mut chat = self.chat.lock().await;
        match chat.as_mut() {
            Some(s) if s.id == owner => s.send(&line).await?,
            _ => {
                // A scheduled job's session: its writer is registered here.
                if let Some(tx) = self.job_writers.lock().await.get(&owner) {
                    let _ = tx.send(line).await;
                } else {
                    anyhow::bail!("the session that asked has ended");
                }
            }
        }
        self.emit(json!({"type": "approval", "action": "answered", "id": id, "approved": approved, "reason": reason, "toolName": req["toolName"], "args": req["args"], "ts": req["answeredAt"]}));
        Ok(())
    }

    async fn chat_session(self: &Arc<Self>) -> anyhow::Result<()> {
        let mut chat = self.chat.lock().await;
        let dead = chat
            .as_ref()
            .map(|s| s.ended.load(std::sync::atomic::Ordering::Relaxed))
            .unwrap_or(true);
        if dead {
            *chat = Some(self.start_session("chat", None, None).await?);
        }
        Ok(())
    }

    async fn send_chat(self: &Arc<Self>, line: Value) -> anyhow::Result<()> {
        self.chat_session().await?;
        let mut chat = self.chat.lock().await;
        chat.as_mut().expect("started above").send(&line).await
    }

    async fn new_chat(self: &Arc<Self>) -> anyhow::Result<String> {
        let mut chat = self.chat.lock().await;
        if let Some(mut old) = chat.take() {
            let _ = old.send(&json!({"type": "end"})).await;
            let _ = tokio::time::timeout(Duration::from_secs(5), old.child.wait()).await;
        }
        let s = self.start_session("chat", None, None).await?;
        let id = s.id.clone();
        *chat = Some(s);
        Ok(id)
    }

    async fn shutdown(&self) {
        if let Some(mut s) = self.chat.lock().await.take() {
            let _ = s.send(&json!({"type": "end"})).await;
            if tokio::time::timeout(Duration::from_secs(5), s.child.wait())
                .await
                .is_err()
            {
                let _ = s.child.kill().await;
            }
        }
    }
}

impl Session {
    async fn send(&mut self, v: &Value) -> anyhow::Result<()> {
        self.stdin.write_all(format!("{v}\n").as_bytes()).await?;
        self.stdin.flush().await?;
        Ok(())
    }
}

// ------------------------------------------------------------- scheduler

/// Fires the agent's schedules while the server runs, each as its own IPC
/// session whose messages the page shows like the chat's.
async fn scheduler(ui: Arc<Ui>) {
    let offset = crate::schedule_cmd::local_offset();
    loop {
        let (found, _) = schedules::discover(&ui.dir);
        let now = now_unix();
        let mut due: Vec<(i64, Schedule)> = found
            .into_iter()
            .filter_map(|s| s.next_after(now - 1, offset).map(|t| (t, s)))
            .collect();
        due.sort_by_key(|(t, _)| *t);
        let Some((at, next)) = due.into_iter().next() else {
            tokio::time::sleep(Duration::from_secs(30)).await;
            continue;
        };
        let wait = (at - now_unix()).clamp(0, 60) as u64;
        if wait > 0 {
            tokio::time::sleep(Duration::from_secs(wait)).await;
            if at > now_unix() {
                continue; // woke early (the list may have changed): look again
            }
            let still = schedules::discover(&ui.dir)
                .0
                .into_iter()
                .find(|s| s.id == next.id);
            match still {
                Some(s) if s.enabled && s.next_after(at - 1, offset) == Some(at) => {}
                _ => continue,
            }
        }
        run_schedule(&ui, &next).await;
    }
}

/// Runs one schedule now in its own session and records the result.
async fn run_schedule(ui: &Arc<Ui>, s: &Schedule) {
    let _one_at_a_time = ui.schedule_lock.lock().await;
    let ts = schedules::utc_now_rfc3339();
    ui.emit(
        json!({"type": "schedule", "action": "start", "id": s.id, "prompt": s.prompt, "ts": ts}),
    );
    let reply = Arc::new(std::sync::Mutex::new(String::new()));
    let outcome = Arc::new(std::sync::Mutex::new(String::new()));
    let (r2, o2) = (reply.clone(), outcome.clone());
    let (done_tx, mut done_rx) = tokio::sync::mpsc::channel::<()>(1);
    let watch: Watcher = Arc::new(move |v: &Value| {
        if v["type"] == "assistant" {
            if let Some(t) = v["content"].as_str() {
                if !t.is_empty() {
                    *r2.lock().unwrap_or_else(|e| e.into_inner()) = t.to_string();
                }
            }
        }
        if v["type"] == "system" && v["subtype"] == "turn_end" {
            *o2.lock().unwrap_or_else(|e| e.into_inner()) =
                v["metadata"]["outcome"].as_str().unwrap_or("").to_string();
            let _ = done_tx.try_send(());
        }
    });
    let (success, result, out_dir) = match ui.start_session(&s.id, None, Some(watch)).await {
        Ok(mut session) => {
            let out_dir = ui.out_dir.join(&session.id);
            let (tx, mut rx) = tokio::sync::mpsc::channel::<Value>(8);
            ui.job_writers.lock().await.insert(session.id.clone(), tx);
            let ok = session
                .send(&json!({"type": "user", "content": s.prompt}))
                .await
                .is_ok();
            // Relay approval answers while the job runs; end the session
            // once its one turn is over.
            let deadline = tokio::time::Instant::now() + Duration::from_secs(3600);
            loop {
                tokio::select! {
                    Some(line) = rx.recv() => { let _ = session.send(&line).await; }
                    Some(()) = done_rx.recv() => { let _ = session.send(&json!({"type": "end"})).await; }
                    status = session.child.wait() => { let _ = status; break; }
                    _ = tokio::time::sleep_until(deadline) => { let _ = session.child.kill().await; break; }
                }
            }
            ui.job_writers.lock().await.remove(&session.id);
            let outcome = outcome.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let reply = reply.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let success = ok && outcome == "completed";
            (
                success,
                if reply.is_empty() { outcome } else { reply },
                out_dir,
            )
        }
        Err(e) => (false, format!("{e:#}"), ui.out_dir.clone()),
    };
    crate::schedule_cmd::record_job(&ui.dir, s, &ts, success, &result, s.once, &out_dir);
    ui.emit(json!({"type": "schedule", "action": "result", "id": s.id, "success": success, "result": crate::schedule_cmd::cut(&result, 2000), "ts": ts}));
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

// ------------------------------------------------------------------ http

struct Request {
    method: String,
    path: String,
    query: BTreeMap<String, String>,
    body: Vec<u8>,
}

async fn serve(mut stream: TcpStream, ui: Arc<Ui>) -> anyhow::Result<()> {
    let req = match read_request(&mut stream).await? {
        Some(r) => r,
        None => return Ok(()),
    };
    if req.method == "GET" && req.path == "/api/events" {
        return sse(stream, ui).await;
    }
    let (status, ctype, body) = match route(&req, &ui).await {
        Ok((ctype, body)) => ("200 OK", ctype, body),
        Err(e) => {
            let (code, msg) = match e.downcast_ref::<HttpError>() {
                Some(HttpError(code, msg)) => (*code, msg.clone()),
                None => (500, format!("{e:#}")),
            };
            let status: &'static str = match code {
                400 => "400 Bad Request",
                404 => "404 Not Found",
                405 => "405 Method Not Allowed",
                _ => "500 Internal Server Error",
            };
            (status, "text/plain; charset=utf-8", msg.into_bytes())
        }
    };
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(&body).await?;
    stream.shutdown().await?;
    Ok(())
}

#[derive(Debug)]
struct HttpError(u16, String);
impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.1)
    }
}
impl std::error::Error for HttpError {}
fn bad(msg: impl Into<String>) -> anyhow::Error {
    HttpError(400, msg.into()).into()
}

async fn read_request(stream: &mut TcpStream) -> anyhow::Result<Option<Request>> {
    let mut buf = Vec::with_capacity(4096);
    let mut tmp = [0u8; 4096];
    let head_end = loop {
        let n = tokio::time::timeout(Duration::from_secs(30), stream.read(&mut tmp)).await??;
        if n == 0 {
            return Ok(None);
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        if buf.len() > 64 * 1024 {
            anyhow::bail!("request head too large");
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let first = lines.next().unwrap_or_default();
    let mut parts = first.split(' ');
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or("/");
    let mut content_length = 0usize;
    for l in lines {
        if let Some((k, v)) = l.split_once(':') {
            if k.eq_ignore_ascii_case("content-length") {
                content_length = v.trim().parse().unwrap_or(0);
            }
        }
    }
    if content_length > 8 * 1024 * 1024 {
        anyhow::bail!("request body too large");
    }
    let mut body = buf[head_end..].to_vec();
    while body.len() < content_length {
        let n = tokio::time::timeout(Duration::from_secs(30), stream.read(&mut tmp)).await??;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
    }
    body.truncate(content_length);
    let (path, qs) = target.split_once('?').unwrap_or((target, ""));
    let mut query = BTreeMap::new();
    for pair in qs.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        query.insert(url_decode(k), url_decode(v));
    }
    Ok(Some(Request {
        method,
        path: url_decode(path),
        query,
        body,
    }))
}

fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => match u8::from_str_radix(&s[i + 1..i + 3], 16) {
                Ok(b) => {
                    out.push(b);
                    i += 3;
                }
                Err(_) => {
                    out.push(b'%');
                    i += 1;
                }
            },
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

async fn sse(mut stream: TcpStream, ui: Arc<Ui>) -> anyhow::Result<()> {
    stream
        .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-store\r\nConnection: keep-alive\r\n\r\n")
        .await?;
    let mut rx = ui.events.subscribe();
    stream.write_all(b": connected\n\n").await?;
    loop {
        tokio::select! {
            msg = rx.recv() => match msg {
                Ok(v) => stream.write_all(format!("data: {v}\n\n").as_bytes()).await?,
                Err(broadcast::error::RecvError::Lagged(_)) => stream.write_all(b"data: {\"type\":\"status\",\"lagged\":true}\n\n").await?,
                Err(_) => break,
            },
            _ = tokio::time::sleep(Duration::from_secs(15)) => stream.write_all(b": keepalive\n\n").await?,
        }
    }
    Ok(())
}

type Reply = (&'static str, Vec<u8>);
const JSON: &str = "application/json; charset=utf-8";

async fn route(req: &Request, ui: &Arc<Ui>) -> anyhow::Result<Reply> {
    let json_body = || -> anyhow::Result<Value> {
        if req.body.is_empty() {
            return Ok(json!({}));
        }
        serde_json::from_slice(&req.body).map_err(|e| bad(format!("the body is not JSON: {e}")))
    };
    let reply = |v: Value| -> anyhow::Result<Reply> { Ok((JSON, serde_json::to_vec(&v)?)) };
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/") | ("GET", "/index.html") => {
            let page = format!(
                "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1, viewport-fit=cover\"></head><body>{PAGE}</body></html>"
            );
            Ok(("text/html; charset=utf-8", page.into_bytes()))
        }
        ("GET", "/api/state") => reply(state(ui).await?),
        ("POST", "/api/send") => {
            let b = json_body()?;
            let text = b["text"]
                .as_str()
                .ok_or_else(|| bad("`text` is required"))?;
            ui.send_chat(json!({"type": "user", "content": text}))
                .await?;
            reply(json!({"ok": true}))
        }
        ("POST", "/api/steer") => {
            let b = json_body()?;
            let text = b["text"]
                .as_str()
                .ok_or_else(|| bad("`text` is required"))?;
            ui.send_chat(json!({"type": "steer", "content": text}))
                .await?;
            reply(json!({"ok": true}))
        }
        ("POST", "/api/abort") => {
            ui.send_chat(json!({"type": "abort"})).await?;
            reply(json!({"ok": true}))
        }
        ("POST", "/api/kill") => {
            ui.send_chat(json!({"type": "kill"})).await?;
            reply(json!({"ok": true}))
        }
        ("POST", "/api/new") => reply(json!({"sessionId": ui.new_chat().await?})),
        ("POST", "/api/approve") => {
            let b = json_body()?;
            let id = b["id"].as_str().ok_or_else(|| bad("`id` is required"))?;
            ui.answer_approval(
                id,
                b["approved"] == Value::Bool(true),
                b["reason"].as_str().unwrap_or(""),
            )
            .await?;
            reply(json!({"ok": true}))
        }
        ("GET", "/api/files") => reply(list_files(
            &ui.dir,
            req.query.get("path").map(String::as_str).unwrap_or(""),
        )?),
        ("GET", "/api/file") => {
            let path = req
                .query
                .get("path")
                .ok_or_else(|| bad("`path` is required"))?;
            reply(read_file(&ui.dir, path)?)
        }
        ("POST", "/api/file") => {
            let b = json_body()?;
            let path = b["path"]
                .as_str()
                .ok_or_else(|| bad("`path` is required"))?;
            let content = b["content"]
                .as_str()
                .ok_or_else(|| bad("`content` is required"))?;
            let full = safe_path(&ui.dir, path)?;
            if let Some(p) = full.parent() {
                std::fs::create_dir_all(p)?;
            }
            std::fs::write(&full, content)?;
            ui.emit(json!({"type": "changes"}));
            reply(json!({"ok": true}))
        }
        ("GET", "/api/prompt") => reply(prompt(ui)?),
        ("GET", "/api/changes") => reply(changes(&ui.dir)),
        ("GET", "/api/change") => {
            let sha = req
                .query
                .get("sha")
                .ok_or_else(|| bad("`sha` is required"))?;
            reply(json!({"text": change(&ui.dir, sha)?}))
        }
        ("POST", "/api/revert") => {
            let b = json_body()?;
            let sha = b["sha"].as_str().ok_or_else(|| bad("`sha` is required"))?;
            let new = revert(&ui.dir, sha)?;
            ui.emit(json!({"type": "changes"}));
            reply(json!({"ok": true, "sha": new}))
        }
        ("GET", "/api/schedules") => reply(json!({"schedules": schedule_list(&ui.dir)})),
        ("POST", "/api/schedule") => {
            let b = json_body()?;
            let id = b["id"].as_str().ok_or_else(|| bad("`id` is required"))?;
            let (found, _) = schedules::discover(&ui.dir);
            let s = found
                .into_iter()
                .find(|s| s.id == id)
                .ok_or_else(|| HttpError(404, format!("no schedule {id:?}")))?;
            match b["action"].as_str() {
                Some("enable") => {
                    schedules::update(&s.path, &[("enabled", Value::Bool(true))]).map_err(bad)?
                }
                Some("disable") => {
                    schedules::update(&s.path, &[("enabled", Value::Bool(false))]).map_err(bad)?
                }
                Some("run") => {
                    let ui = ui.clone();
                    tokio::spawn(async move { run_schedule(&ui, &s).await });
                }
                other => return Err(bad(format!("unknown action {other:?}"))),
            }
            reply(json!({"ok": true}))
        }
        ("GET", "/api/sessions") => reply(json!({"sessions": session_list(&ui.dir)})),
        ("GET", "/api/session") => {
            let id = req.query.get("id").ok_or_else(|| bad("`id` is required"))?;
            reply(session_detail(&ui.dir, id)?)
        }
        ("GET", _) => Err(HttpError(404, "not found".into()).into()),
        _ => Err(HttpError(405, "method not allowed".into()).into()),
    }
}

// ------------------------------------------------------------- handlers

async fn state(ui: &Arc<Ui>) -> anyhow::Result<Value> {
    let spec = load_spec(ui)?;
    let (name, description) = match &spec.agent {
        Some(a) => (a.manifest.name.clone(), a.manifest.description.clone()),
        None => (spec.config.name.clone(), String::new()),
    };
    let branch = git(&ui.dir, &["rev-parse", "--abbrev-ref", "HEAD"])
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let chat = ui.chat.lock().await;
    let (session, messages) = match chat.as_ref() {
        Some(s) => (
            json!({"id": s.id, "status": if s.running.load(std::sync::atomic::Ordering::Relaxed) { "running" } else { "idle" }}),
            s.messages.lock().await.clone(),
        ),
        None => (json!({"id": "", "status": "idle"}), Vec::new()),
    };
    drop(chat);
    let approvals = ui.approvals.lock().await;
    Ok(json!({
        "agent": {"name": name, "description": description, "dir": ui.dir, "model": spec.model.to_string(), "branch": branch},
        "tools": gitagent_core::offered_tools(&spec, None, None, None, &[]).into_iter().map(|t| gitagent_loader::v1::v1_name(&t.name).to_string()).collect::<Vec<_>>(),
        "skills": spec.skills.iter().map(|k| json!({"name": k.name, "description": k.description})).collect::<Vec<_>>(),
        "session": session,
        "messages": messages,
        "approvals": approvals.pending,
        "history": approvals.history,
        "schedules": schedule_list(&ui.dir),
        "sessions": session_list(&ui.dir),
        "changes": changes(&ui.dir),
    }))
}

fn load_spec(ui: &Ui) -> anyhow::Result<gitagent_loader::AgentSpec> {
    gitagent_loader::load(&gitagent_loader::LoadOptions {
        config_path: None,
        project_dir: Some(ui.dir.clone()),
        home_dir: std::env::var_os("HOME").map(PathBuf::from),
        model: ui.model.clone(),
        profile: None,
        conversation: true,
        env: ui.env.clone(),
    })
}

fn prompt(ui: &Ui) -> anyhow::Result<Value> {
    let spec = load_spec(ui)?;
    let text = &spec.system_prompt;
    let mut sections: Vec<Value> = Vec::new();
    let mut title = String::from("instructions");
    let mut buf = String::new();
    let flush = |title: &str, buf: &str, sections: &mut Vec<Value>| {
        let t = buf.trim();
        if !t.is_empty() {
            sections.push(
                json!({"title": title.to_lowercase(), "source": source_of(title), "text": t}),
            );
        }
    };
    for line in text.lines() {
        if let Some(h) = line.strip_prefix("# ") {
            flush(&title, &buf, &mut sections);
            title = h.trim().to_string();
            buf.clear();
        } else {
            buf.push_str(line);
            buf.push('\n');
        }
    }
    flush(&title, &buf, &mut sections);
    // The first section is the identity (`# <name> v<version>`).
    if let Some(first) = sections.first_mut() {
        if first["source"] == "" {
            first["source"] = json!("SOUL.md");
        }
    }
    Ok(json!({"chars": text.chars().count(), "sections": sections}))
}

fn source_of(title: &str) -> &'static str {
    let t = title.to_lowercase();
    if t.contains("identity") {
        "SOUL.md"
    } else if t.contains("rules") {
        "RULES.md"
    } else if t.contains("duties") {
        "DUTIES.md"
    } else if t.contains("lesson") {
        "prompts/lessons.md"
    } else if t.contains("skill") {
        "skills/"
    } else if t.contains("memory") {
        "memory/"
    } else if t.contains("knowledge") {
        "knowledge/"
    } else if t.contains("workflow") {
        "workflows/"
    } else if t.contains("sub-agent") || t.contains("agents") {
        "agents/"
    } else if t.contains("compliance") {
        "agent.yaml"
    } else if t.contains("example") {
        "examples/"
    } else {
        ""
    }
}

fn safe_path(root: &Path, rel: &str) -> anyhow::Result<PathBuf> {
    let rel = rel.trim_start_matches('/');
    if rel.is_empty() {
        return Ok(root.to_path_buf());
    }
    let p = Path::new(rel);
    if p.components()
        .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return Err(bad("the path must stay inside the agent directory"));
    }
    if rel == ".git" || rel.starts_with(".git/") {
        return Err(bad("the git database is not served"));
    }
    Ok(root.join(p))
}

fn list_files(root: &Path, rel: &str) -> anyhow::Result<Value> {
    let dir = safe_path(root, rel)?;
    let mut entries: Vec<Value> = Vec::new();
    for e in std::fs::read_dir(&dir)? {
        let e = e?;
        let name = e.file_name().to_string_lossy().into_owned();
        if name == ".git" || name == "node_modules" || name == "target" {
            continue;
        }
        let meta = e.metadata()?;
        let path = if rel.is_empty() {
            name.clone()
        } else {
            format!("{}/{name}", rel.trim_end_matches('/'))
        };
        entries.push(json!({"name": name, "dir": meta.is_dir(), "path": path, "size": meta.len()}));
    }
    entries.sort_by(|a, b| {
        b["dir"].as_bool().cmp(&a["dir"].as_bool()).then_with(|| {
            a["name"]
                .as_str()
                .unwrap_or("")
                .to_lowercase()
                .cmp(&b["name"].as_str().unwrap_or("").to_lowercase())
        })
    });
    Ok(json!({"entries": entries}))
}

fn read_file(root: &Path, rel: &str) -> anyhow::Result<Value> {
    let p = safe_path(root, rel)?;
    if !p.is_file() {
        return Ok(json!({"kind": "missing"}));
    }
    let bytes = std::fs::read(&p)?;
    if bytes.len() > 2 * 1024 * 1024 {
        return Ok(json!({"kind": "binary", "size": bytes.len()}));
    }
    match String::from_utf8(bytes) {
        Ok(text) => Ok(json!({"kind": "text", "content": text, "size": text.len()})),
        Err(e) => Ok(json!({"kind": "binary", "size": e.as_bytes().len()})),
    }
}

fn git(dir: &Path, args: &[&str]) -> anyhow::Result<String> {
    let out = std::process::Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()?;
    if !out.status.success() {
        anyhow::bail!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn changes(dir: &Path) -> Value {
    let uncommitted: Vec<Value> = git(
        dir,
        &[
            "status",
            "--porcelain",
            "--untracked-files=all",
            "--",
            ".",
            ":(exclude).gitagent",
        ],
    )
    .unwrap_or_default()
    .lines()
    .filter_map(|l| {
        let (status, path) = l.split_at_checked(3)?;
        Some(json!({"status": status.trim(), "path": path.trim()}))
    })
    .collect();
    let commits: Vec<Value> = git(dir, &["log", "-30", "--format=%H%x09%s%x09%ar%x09%an"])
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let mut f = l.split('\t');
            Some(json!({"sha": f.next()?, "subject": f.next()?, "when": f.next()?, "author": f.next().unwrap_or("")}))
        })
        .collect();
    json!({"uncommitted": uncommitted, "commits": commits})
}

fn change(dir: &Path, sha: &str) -> anyhow::Result<String> {
    if sha == "working" {
        let tracked =
            git(dir, &["diff", "HEAD", "--", ".", ":(exclude).gitagent"]).unwrap_or_default();
        let untracked = git(
            dir,
            &[
                "ls-files",
                "--others",
                "--exclude-standard",
                "--",
                ".",
                ":(exclude).gitagent",
            ],
        )
        .unwrap_or_default();
        let mut text = tracked;
        for f in untracked.lines() {
            if let Ok(content) = std::fs::read_to_string(dir.join(f)) {
                text.push_str(&format!(
                    "diff --git a/{f} b/{f}\nnew file\n--- /dev/null\n+++ b/{f}\n"
                ));
                for l in content.lines() {
                    text.push('+');
                    text.push_str(l);
                    text.push('\n');
                }
            }
        }
        return Ok(text);
    }
    if !sha.chars().all(|c| c.is_ascii_hexdigit()) || sha.len() < 7 {
        return Err(bad("not a commit id"));
    }
    git(dir, &["show", "--format=%H%n%an · %ar%n%s%n", sha])
}

fn revert(dir: &Path, sha: &str) -> anyhow::Result<String> {
    if !sha.chars().all(|c| c.is_ascii_hexdigit()) || sha.len() < 7 {
        return Err(bad("not a commit id"));
    }
    git(
        dir,
        &[
            "-c",
            "user.name=gitagent",
            "-c",
            "user.email=gitagent@localhost",
            "revert",
            "--no-edit",
            sha,
        ],
    )?;
    Ok(git(dir, &["rev-parse", "HEAD"])?.trim().to_string())
}

fn schedule_list(dir: &Path) -> Vec<Value> {
    let offset = crate::schedule_cmd::local_offset();
    let now = now_unix();
    schedules::discover(dir)
        .0
        .into_iter()
        .map(|s| {
            json!({
                "id": s.id, "description": s.description, "when": s.when(), "enabled": s.enabled,
                "next": s.next_after(now, offset).map(|t| schedules::format_local(t, offset)),
                "lastRunAt": s.last_run_at, "lastResult": s.last_result, "prompt": s.prompt,
            })
        })
        .collect()
}

fn session_list(dir: &Path) -> Vec<Value> {
    let mut out: Vec<Value> = std::fs::read_dir(dir.join(".gitagent/sessions"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| std::fs::read_to_string(e.path().join("ui.json")).ok())
        .filter_map(|t| serde_json::from_str::<Value>(&t).ok())
        .collect();
    out.sort_by(|a, b| b["started"].as_str().cmp(&a["started"].as_str()));
    out.truncate(100);
    out
}

fn session_detail(dir: &Path, id: &str) -> anyhow::Result<Value> {
    if id.is_empty()
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
        || id.starts_with('.')
    {
        return Err(bad("not a session id"));
    }
    let d = dir.join(".gitagent/sessions").join(id);
    let mut meta: Value = std::fs::read_to_string(d.join("ui.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| json!({"id": id}));
    let messages: Vec<Value> = std::fs::read_to_string(d.join("ui.jsonl"))
        .map_err(|_| HttpError(404, format!("no session {id:?}")))?
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    meta["messages"] = json!(messages);
    Ok(meta)
}
