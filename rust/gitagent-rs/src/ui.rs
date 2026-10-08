//! Ira desktop UI — a single-binary local web app. A tiny embedded HTTP server
//! serves the (embedded) Heritage-Premium chat frontend and streams SDK events
//! over Server-Sent Events. `ira ui` starts it and opens the browser. No JS
//! toolchain, no app bundle — it all ships inside the `ira` binary.

use futures_util::StreamExt;
use crate::sdk::query::{open_session, Event, Session};
use serde_json::json;
use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use tiny_http::{Header, Response, Server, StatusCode};

const INDEX_HTML: &str = include_str!("ui/index.html");

/// Vendored CodeMirror (MIT) — served from the binary, no CDN at runtime.
const VENDOR: &[(&str, &str, &[u8])] = &[
    ("/vendor/cm.js", "text/javascript", include_bytes!("ui/vendor/cm.js")),
    ("/vendor/cm.css", "text/css", include_bytes!("ui/vendor/cm.css")),
    ("/vendor/mode-yaml.js", "text/javascript", include_bytes!("ui/vendor/mode-yaml.js")),
    ("/vendor/mode-markdown.js", "text/javascript", include_bytes!("ui/vendor/mode-markdown.js")),
    ("/vendor/mode-javascript.js", "text/javascript", include_bytes!("ui/vendor/mode-javascript.js")),
    ("/vendor/mode-python.js", "text/javascript", include_bytes!("ui/vendor/mode-python.js")),
    ("/vendor/mode-shell.js", "text/javascript", include_bytes!("ui/vendor/mode-shell.js")),
];

/// Config shared by every session created from the UI.
struct Cfg {
    dir: PathBuf,
    model: Option<String>,
    permission_mode: Option<String>,
    /// The UI page with brand placeholders resolved (IRA_BRAND env, default "Ira Agentic").
    html: String,
    /// Per-launch API token; required on every /api, /v1 and /raw request.
    token: String,
    /// The bound port (Host/Origin must name it).
    port: u16,
}

/// A fresh random API token for this launch (`IRA_UI_TOKEN` pins one, e.g. for
/// external apps calling /v1 across restarts; it must be at least 16 chars).
fn launch_token() -> anyhow::Result<String> {
    if let Ok(t) = std::env::var("IRA_UI_TOKEN") {
        let t = t.trim().to_string();
        if t.len() >= 16 && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            return Ok(t);
        }
        eprintln!("warning: ignoring IRA_UI_TOKEN (needs 16+ chars of [A-Za-z0-9_-])");
    }
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).map_err(|e| anyhow::anyhow!("could not generate a UI token: {e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Constant-time string equality (token comparison).
fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Endpoints that change state and take a JSON body: POST + application/json.
const POST_JSON_PATHS: &[&str] = &[
    "/api/chat",
    "/api/agents/create",
    "/api/agents/update",
    "/api/agents/delete",
    "/api/studio/skills/install",
    "/v1/chat/completions",
];
/// Endpoints that change state (or launch processes) without a JSON body: POST.
const POST_PATHS: &[&str] = &[
    "/api/file/save",
    "/api/upload",
    "/api/onboarding/done",
    "/api/open",
    "/api/openurl",
    "/api/auth/browser",
    "/api/ollama/pull",
    "/api/ollama/pull/cancel",
    "/api/ollama/delete",
];

/// Gate every request before routing. `headers` are (lower-cased name, value).
/// - Host must be this server (127.0.0.1:<port> or localhost:<port>) — blocks
///   DNS rebinding.
/// - A present Origin must be this server — blocks cross-site requests.
/// - Everything except the page itself and vendored assets needs the launch
///   token (X-Ira-Token or `Authorization: Bearer`; `/raw` GETs may pass it as
///   `?token=` because <img>/<iframe> can't set headers).
/// - State-changing endpoints require POST (and JSON where they take a body).
/// Returns (status, message) when the request must be rejected.
fn authorize(
    method: &str,
    url: &str,
    headers: &[(String, String)],
    port: u16,
    token: &str,
) -> Result<(), (u16, &'static str)> {
    let header = |name: &str| headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.trim());
    let host_ok = header("host")
        .map(|h| {
            let h = h.to_ascii_lowercase();
            h == format!("127.0.0.1:{port}") || h == format!("localhost:{port}")
        })
        .unwrap_or(false);
    if !host_ok {
        return Err((403, "forbidden host"));
    }
    if let Some(origin) = header("origin") {
        let o = origin.to_ascii_lowercase();
        if o != format!("http://127.0.0.1:{port}") && o != format!("http://localhost:{port}") {
            return Err((403, "forbidden origin"));
        }
    }

    let path = url.split('?').next().unwrap_or("");
    let public = path == "/" || path == "/index.html" || VENDOR.iter().any(|(p, _, _)| path == *p);
    if public {
        return if method == "GET" || method == "HEAD" { Ok(()) } else { Err((405, "method not allowed")) };
    }

    let bearer = header("authorization").and_then(|v| {
        let (scheme, rest) = v.split_once(' ')?;
        scheme.eq_ignore_ascii_case("bearer").then(|| rest.trim())
    });
    let presented = header("x-ira-token").or(bearer).map(str::to_string).or_else(|| {
        (path == "/raw" && method == "GET").then(|| query_param(url, "token")).flatten()
    });
    match presented {
        Some(t) if ct_eq(&t, token) => {}
        _ => return Err((401, "missing or invalid API token")),
    }

    let json_body = POST_JSON_PATHS.contains(&path) || (path == "/api/auth/handoff" && method == "POST");
    if (json_body || POST_PATHS.contains(&path)) && method != "POST" {
        return Err((405, "method not allowed"));
    }
    if json_body {
        let ct = header("content-type").unwrap_or("").to_ascii_lowercase();
        if !ct.starts_with("application/json") {
            return Err((415, "expected application/json"));
        }
    }
    Ok(())
}

/// A model string sent over the HTTP API may only override the endpoint with a
/// loopback URL (local Ollama etc.); remote `@baseurl`s must be configured
/// locally (agent.yaml / GITAGENT_MODEL_BASE_URL), never by a request.
fn validate_model_override(model: &str) -> Result<(), String> {
    let rest = model.split_once(':').map(|(_, r)| r).unwrap_or(model);
    match rest.split_once('@') {
        None => Ok(()),
        Some((_, base)) if crate::pi::provider::is_loopback_base_url(base) => Ok(()),
        Some(_) => Err("model endpoint overrides (@baseurl) must point at localhost".into()),
    }
}

/// Resolve brand placeholders: `IRA_BRAND="Lyzr Edgespace"` → h1 "Lyzr",
/// eyebrow "Edgespace", title "Lyzr Edgespace". The assistant's display name
/// comes from the agent manifest's `name` field.
fn branded_html(dir: &std::path::Path, token: &str) -> String {
    let brand = std::env::var("IRA_BRAND").unwrap_or_else(|_| "Ira Agentic".to_string());
    let (name, tag) = brand.split_once(' ').unwrap_or((brand.as_str(), ""));
    let agent = crate::sdk::loader::load_agent(dir)
        .map(|l| l.manifest.name)
        .unwrap_or_else(|_| "Ira".to_string());
    // Memberstack auth (same app as agent-studio-ui; override via env).
    let ms_key = std::env::var("IRA_MS_KEY").unwrap_or_else(|_| "pk_c14a2728e715d9ea67bf".into());
    let ms_plan = std::env::var("IRA_MS_PLAN").unwrap_or_else(|_| "pln_free-jx6p0t59".into());
    INDEX_HTML
        .replace("__TITLE__", &brand)
        .replace("__BRAND__", name)
        .replace("__TAG__", tag)
        .replace("__AGENT_JS__", &serde_json::Value::String(agent).to_string())
        .replace("__MS_KEY__", &ms_key)
        .replace("__MS_PLAN__", &ms_plan)
        .replace("__IRA_TOKEN__", token)
}

/// Start the UI server and return its URL. The accept loop runs on a background
/// task — callers embed the URL (Tauri webview) or open a browser (`open`).
pub async fn start_server(
    dir: PathBuf,
    model: Option<String>,
    permission_mode: Option<String>,
    open: bool,
) -> anyhow::Result<String> {
    let handle = tokio::runtime::Handle::current();
    let sessions: Arc<Mutex<HashMap<String, Session>>> = Arc::new(Mutex::new(HashMap::new()));
    // Take over the canonical port: a stale `ira` from a previous build serving
    // OLD code on 8787 is the #1 cause of "my fixes aren't there". Kill any ira
    // holding a port in our range, then bind.
    kill_stale_instances();
    let (server, addr) = (8787u16..8797)
        .find_map(|p| {
            let a = format!("127.0.0.1:{p}");
            Server::http(&a).ok().map(|s| (s, a))
        })
        .ok_or_else(|| anyhow::anyhow!("could not bind a local port (8787-8796)"))?;
    let port = server.server_addr().to_ip().map(|a| a.port()).unwrap_or(0);
    let token = launch_token()?;
    let html = branded_html(&dir, &token);
    let cfg = Arc::new(Cfg { dir, model, permission_mode, html, token, port });
    let url = format!("http://{addr}");
    *server_base_cell().lock().unwrap_or_else(|e| e.into_inner()) = Some(url.clone());
    println!("Ira UI v{} → {url}", env!("CARGO_PKG_VERSION"));
    if std::env::var("IRA_UI_PRINT_TOKEN").is_ok() {
        println!("API token (for /v1 clients): {}", cfg.token);
    }
    warm_model(&cfg); // preload local model weights so the first reply is fast
    if open {
        open_browser(&url);
    }

    let server = Arc::new(server);
    // Blocking accept loop off the async worker; one thread per request so a
    // long-lived SSE stream never blocks page/asset requests.
    tokio::task::spawn_blocking(move || {
        for request in server.incoming_requests() {
            let sessions = sessions.clone();
            let cfg = cfg.clone();
            let handle = handle.clone();
            std::thread::spawn(move || handle_request(request, sessions, cfg, handle));
        }
    });
    Ok(url)
}

/// CLI entry: start the server, open the browser, serve forever.
pub async fn run_ui(dir: PathBuf, model: Option<String>, permission_mode: Option<String>) -> i32 {
    match start_server(dir, model, permission_mode, true).await {
        Ok(_) => {
            futures_util::future::pending::<()>().await;
            0
        }
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

fn handle_request(
    mut request: tiny_http::Request,
    sessions: Arc<Mutex<HashMap<String, Session>>>,
    cfg: Arc<Cfg>,
    handle: tokio::runtime::Handle,
) {
    let url = request.url().to_string();

    let headers: Vec<(String, String)> = request
        .headers()
        .iter()
        .map(|h| (h.field.as_str().as_str().to_ascii_lowercase(), h.value.as_str().to_string()))
        .collect();
    if let Err((code, msg)) = authorize(request.method().as_str(), &url, &headers, cfg.port, &cfg.token) {
        let hdr = Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap();
        let _ = request.respond(
            Response::from_string(json!({ "error": msg }).to_string()).with_status_code(code).with_header(hdr),
        );
        return;
    }

    if let Some((_, ct, bytes)) = VENDOR.iter().find(|(p, _, _)| url.starts_with(p)) {
        let ct_hdr = Header::from_bytes(&b"Content-Type"[..], ct.as_bytes()).unwrap();
        let cache = Header::from_bytes(&b"Cache-Control"[..], &b"max-age=86400"[..]).unwrap();
        let _ = request.respond(Response::from_data(bytes.to_vec()).with_header(ct_hdr).with_header(cache));
        return;
    }

    if url.starts_with("/api/file/save?") && *request.method() == tiny_http::Method::Post {
        let rel = query_param(&url, "path").unwrap_or_default();
        let adir = resolve_agent(&cfg, &query_param(&url, "agent").unwrap_or_default());
        let mut buf = Vec::new();
        let _ = request.as_reader().take(5 * 1024 * 1024 + 1).read_to_end(&mut buf);
        let body = if buf.len() > 5 * 1024 * 1024 {
            json!({ "error": "file exceeds the 5 MB editor limit" }).to_string()
        } else {
            save_text_file(&adir, &rel, &buf)
        };
        let hdr = Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap();
        let _ = request.respond(Response::from_string(body).with_header(hdr));
        return;
    }

    if url.starts_with("/api/upload") && *request.method() == tiny_http::Method::Post {
        let name = query_param(&url, "name").unwrap_or_else(|| "file".into());
        let adir = resolve_agent(&cfg, &query_param(&url, "agent").unwrap_or_default());
        let body = save_upload(&adir, &name, &mut request);
        let hdr = Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap();
        let _ = request.respond(Response::from_string(body).with_header(hdr));
        return;
    }

    if url == "/" || url.starts_with("/?") || url == "/index.html" {
        let ct = Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..]).unwrap();
        // Never cache the UI — otherwise a rebuilt binary serves stale JS/CSS.
        let cc = Header::from_bytes(&b"Cache-Control"[..], &b"no-store, must-revalidate"[..]).unwrap();
        let _ = request.respond(Response::from_string(cfg.html.clone()).with_header(ct).with_header(cc));
        return;
    }

    if url.starts_with("/api/files") {
        let rel = query_param(&url, "path").unwrap_or_default();
        let adir = resolve_agent(&cfg, &query_param(&url, "agent").unwrap_or_default());
        let spec_only = query_param(&url, "spec").as_deref() == Some("1");
        let body = list_files(&adir, &rel, spec_only);
        let hdr = Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap();
        let _ = request.respond(Response::from_string(body).with_header(hdr));
        return;
    }

    if url.starts_with("/api/file?") {
        let rel = query_param(&url, "path").unwrap_or_default();
        let adir = resolve_agent(&cfg, &query_param(&url, "agent").unwrap_or_default());
        let body = file_meta(&adir, &rel);
        let hdr = Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap();
        let _ = request.respond(Response::from_string(body).with_header(hdr));
        return;
    }

    if url.starts_with("/raw?") {
        let rel = query_param(&url, "path").unwrap_or_default();
        let adir = resolve_agent(&cfg, &query_param(&url, "agent").unwrap_or_default());
        match safe_join(&adir, &rel).filter(|p| p.is_file()).and_then(|p| std::fs::read(&p).ok().map(|b| (p, b))) {
            Some((p, bytes)) => {
                let ct = content_type_for(&p);
                let hdr = Header::from_bytes(&b"Content-Type"[..], ct.as_bytes()).unwrap();
                let _ = request.respond(Response::from_data(bytes).with_header(hdr));
            }
            None => {
                let _ = request.respond(Response::from_string("not found").with_status_code(404));
            }
        }
        return;
    }

    if url.starts_with("/api/open?") {
        // Open with the OS default app (files the in-UI viewer can't render).
        let rel = query_param(&url, "path").unwrap_or_default();
        let adir = resolve_agent(&cfg, &query_param(&url, "agent").unwrap_or_default());
        let ok = safe_join(&adir, &rel)
            .map(|p| {
                let cmd = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
                std::process::Command::new(cmd).arg(&p).spawn().is_ok()
            })
            .unwrap_or(false);
        let _ = request.respond(Response::from_string(if ok { "ok" } else { "fail" }));
        return;
    }

    // ── First-run onboarding ────────────────────────────────────────────
    if url.starts_with("/api/onboarding/done") && *request.method() == tiny_http::Method::Post {
        let home = agents_root().parent().map(|p| p.to_path_buf()).unwrap_or_default();
        let _ = std::fs::create_dir_all(&home);
        let ok = std::fs::write(home.join("onboarded"), b"1").is_ok();
        let hdr = Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap();
        let _ = request.respond(Response::from_string(json!({ "ok": ok }).to_string()).with_header(hdr));
        return;
    }
    if url.starts_with("/api/onboarding") {
        let done = agents_root().parent().map(|p| p.join("onboarded").exists()).unwrap_or(true);
        let hdr = Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap();
        let _ = request.respond(Response::from_string(json!({ "done": done }).to_string()).with_header(hdr));
        return;
    }
    if url.starts_with("/api/pickfolder") {
        // Native folder chooser; requests run on their own thread so blocking is fine.
        let body = if cfg!(target_os = "macos") {
            match std::process::Command::new("osascript")
                .arg("-e")
                .arg("POSIX path of (choose folder with prompt \"Choose a workspace folder for your agent\")")
                .output()
            {
                Ok(o) if o.status.success() => {
                    let p = String::from_utf8_lossy(&o.stdout).trim().to_string();
                    json!({ "path": p }).to_string()
                }
                Ok(_) => json!({ "error": "cancelled" }).to_string(),
                Err(e) => json!({ "error": e.to_string() }).to_string(),
            }
        } else {
            json!({ "error": "folder picker is only available on macOS" }).to_string()
        };
        let hdr = Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap();
        let _ = request.respond(Response::from_string(body).with_header(hdr));
        return;
    }
    if url.starts_with("/api/openurl?") {
        let u = query_param(&url, "u").unwrap_or_default();
        let ok = u.starts_with("https://") && {
            let cmd = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
            std::process::Command::new(cmd).arg(&u).spawn().is_ok()
        };
        let hdr = Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap();
        let _ = request.respond(Response::from_string(json!({ "ok": ok }).to_string()).with_header(hdr));
        return;
    }

    // ── OAuth handoff: desktop webviews can't open popups, so Google login
    //    runs in the system browser and the session token is handed back. ──
    if url.starts_with("/api/auth/browser") {
        // Open the app (gauth mode) in the user's default browser.
        let base = server_base().unwrap_or_else(|| "http://127.0.0.1:8787".into());
        let target = format!("{base}/?gauth=1");
        let cmd = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
        let ok = std::process::Command::new(cmd).arg(&target).spawn().is_ok();
        let _ = request.respond(Response::from_string(json!({ "ok": ok }).to_string()));
        return;
    }
    if url.starts_with("/api/auth/handoff") && *request.method() == tiny_http::Method::Post {
        let mut buf = Vec::new();
        let _ = request.as_reader().take(64 * 1024).read_to_end(&mut buf);
        let token = serde_json::from_slice::<serde_json::Value>(&buf)
            .ok()
            .and_then(|v| v.get("token").and_then(|t| t.as_str()).map(String::from))
            .unwrap_or_default();
        let ok = !token.is_empty();
        if ok {
            *handoff().lock().unwrap_or_else(|e| e.into_inner()) = Some(token);
        }
        let _ = request.respond(Response::from_string(json!({ "ok": ok }).to_string()));
        return;
    }
    if url.starts_with("/api/auth/handoff") {
        // One-shot read: the app consumes the token exactly once.
        let token = handoff().lock().unwrap_or_else(|e| e.into_inner()).take();
        let hdr = Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap();
        let _ = request.respond(Response::from_string(json!({ "token": token }).to_string()).with_header(hdr));
        return;
    }

    // ── Studio skills: same auth chain + catalog as agent-studio-ui ─────
    if url.starts_with("/api/studio/skills/installed") {
        let adir = resolve_agent(&cfg, &query_param(&url, "agent").unwrap_or_default());
        let body = installed_skills(&adir);
        let hdr = Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap();
        let _ = request.respond(Response::from_string(body).with_header(hdr));
        return;
    }
    if url.starts_with("/api/studio/skills/install") && *request.method() == tiny_http::Method::Post {
        let mut buf = Vec::new();
        let _ = request.as_reader().take(256 * 1024).read_to_end(&mut buf);
        let body = install_studio_skill(&cfg, &buf);
        let hdr = Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap();
        let _ = request.respond(Response::from_string(body).with_header(hdr));
        return;
    }
    if url.starts_with("/api/studio/skills") {
        let token = request
            .headers()
            .iter()
            .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case("x-ms-token"))
            .map(|h| h.value.as_str().to_string())
            .unwrap_or_default();
        let search = query_param(&url, "search").unwrap_or_default();
        let body = handle.block_on(studio_list_skills(&token, &search));
        let hdr = Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap();
        let _ = request.respond(Response::from_string(body).with_header(hdr));
        return;
    }

    // ── Agents: list / create / resolve ─────────────────────────────────
    if url.starts_with("/api/agents/create") && *request.method() == tiny_http::Method::Post {
        let mut buf = Vec::new();
        let _ = request.as_reader().take(1024 * 1024).read_to_end(&mut buf);
        let body = create_agent(&handle, &buf);
        let hdr = Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap();
        let _ = request.respond(Response::from_string(body).with_header(hdr));
        return;
    }
    if url.starts_with("/api/agents/get?") {
        let id = query_param(&url, "id").unwrap_or_default();
        let body = agent_detail(&cfg, &id);
        let hdr = Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap();
        let _ = request.respond(Response::from_string(body).with_header(hdr));
        return;
    }
    if url.starts_with("/api/agents/update") && *request.method() == tiny_http::Method::Post {
        let mut buf = Vec::new();
        let _ = request.as_reader().take(1024 * 1024).read_to_end(&mut buf);
        let body = update_agent(&cfg, &buf);
        let hdr = Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap();
        let _ = request.respond(Response::from_string(body).with_header(hdr));
        return;
    }
    if url.starts_with("/api/agents/delete") && *request.method() == tiny_http::Method::Post {
        let mut buf = Vec::new();
        let _ = request.as_reader().take(64 * 1024).read_to_end(&mut buf);
        let id = serde_json::from_slice::<serde_json::Value>(&buf)
            .ok()
            .and_then(|v| v.get("id").and_then(|s| s.as_str()).map(String::from))
            .unwrap_or_default();
        let body = delete_agent(&id);
        let hdr = Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap();
        let _ = request.respond(Response::from_string(body).with_header(hdr));
        return;
    }
    if url.starts_with("/api/agents") {
        let body = list_agents(&cfg);
        let hdr = Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap();
        let _ = request.respond(Response::from_string(body).with_header(hdr));
        return;
    }

    // ── OpenAI-compatible endpoint: connect agents to OTHER apps ─────────
    // POST /v1/chat/completions with {"model": "<agent name>", "messages": [...]}.
    if url.starts_with("/v1/chat/completions") && *request.method() == tiny_http::Method::Post {
        let mut buf = Vec::new();
        let _ = request.as_reader().take(10 * 1024 * 1024).read_to_end(&mut buf);
        let Ok(body) = serde_json::from_slice::<serde_json::Value>(&buf) else {
            let _ = request.respond(Response::from_string(r#"{"error":"invalid json"}"#).with_status_code(400));
            return;
        };
        let agent = body.get("model").and_then(|m| m.as_str()).unwrap_or("").to_string();
        let stream = body.get("stream").and_then(|s| s.as_bool()).unwrap_or(false);
        let prompt = body
            .get("messages")
            .and_then(|m| m.as_array())
            .and_then(|arr| arr.iter().rev().find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user")))
            .and_then(|m| m.get("content").and_then(|c| c.as_str()))
            .unwrap_or("")
            .to_string();
        if prompt.is_empty() {
            let _ = request.respond(Response::from_string(r#"{"error":"no user message"}"#).with_status_code(400));
            return;
        }
        let dir = resolve_agent(&cfg, &agent);
        serve_openai_completion(request, dir, agent, prompt, stream, handle);
        return;
    }

    // ── Model management (proxied to the local Ollama) ──────────────────
    if url.starts_with("/api/ollama/setup") {
        // Status poll; `?start=1` (or first-ever call) also kicks the setup.
        let force = url.contains("start=1");
        let should = {
            let st = engine().lock().unwrap_or_else(|e| e.into_inner());
            match st.phase.as_str() {
                "idle" => true,
                "error" | "ready" => force,
                _ => false,
            }
        };
        if should {
            start_engine_setup(&handle);
        }
        let body = {
            let st = engine().lock().unwrap_or_else(|e| e.into_inner());
            json!({ "phase": st.phase, "total": st.total, "completed": st.completed, "error": st.error }).to_string()
        };
        let hdr = Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap();
        let _ = request.respond(Response::from_string(body).with_header(hdr));
        return;
    }
    if url.starts_with("/api/ollama/tags") || url.starts_with("/api/ollama/ps") {
        let endpoint = if url.contains("/ps") { "ps" } else { "tags" };
        let body = handle.block_on(async {
            match reqwest::Client::new()
                .get(format!("http://localhost:11434/api/{endpoint}"))
                .timeout(std::time::Duration::from_secs(4))
                .send()
                .await
            {
                Ok(r) => r.text().await.unwrap_or_else(|_| "{}".into()),
                Err(_) => json!({ "error": "ollama not running" }).to_string(),
            }
        });
        let hdr = Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap();
        let _ = request.respond(Response::from_string(body).with_header(hdr));
        return;
    }

    if url.starts_with("/api/ollama/pull/cancel?") {
        let name = query_param(&url, "name").unwrap_or_default();
        {
            let mut map = pulls().lock().unwrap_or_else(|e| e.into_inner());
            if let Some(st) = map.get_mut(&name) {
                st.cancel = true;
            }
        }
        let hdr = Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap();
        let _ = request.respond(Response::from_string(json!({ "ok": true }).to_string()).with_header(hdr));
        return;
    }
    if url.starts_with("/api/ollama/pull?") {
        let name = query_param(&url, "name").unwrap_or_default();
        let body = if name.is_empty() {
            json!({ "error": "missing name" }).to_string()
        } else {
            start_pull(&handle, &name);
            json!({ "ok": true }).to_string()
        };
        let hdr = Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap();
        let _ = request.respond(Response::from_string(body).with_header(hdr));
        return;
    }

    if url.starts_with("/api/ollama/pulls") {
        let body = {
            let map = pulls().lock().unwrap_or_else(|e| e.into_inner());
            let items: Vec<serde_json::Value> = map
                .iter()
                .map(|(k, v)| {
                    json!({ "name": k, "status": v.status, "total": v.total,
                            "completed": v.completed, "done": v.done, "error": v.error })
                })
                .collect();
            json!({ "pulls": items }).to_string()
        };
        let hdr = Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap();
        let _ = request.respond(Response::from_string(body).with_header(hdr));
        return;
    }

    if url.starts_with("/api/ollama/delete?") {
        let name = query_param(&url, "name").unwrap_or_default();
        let body = handle.block_on(async {
            match reqwest::Client::new()
                .delete("http://localhost:11434/api/delete")
                .json(&json!({ "name": name }))
                .send()
                .await
            {
                Ok(r) if r.status().is_success() => json!({ "ok": true }).to_string(),
                Ok(r) => json!({ "error": format!("HTTP {}", r.status()) }).to_string(),
                Err(e) => json!({ "error": e.to_string() }).to_string(),
            }
        });
        let hdr = Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap();
        let _ = request.respond(Response::from_string(body).with_header(hdr));
        return;
    }

    if url.starts_with("/api/models") {
        let body = handle.block_on(list_models(&cfg));
        let hdr = Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap();
        let _ = request.respond(Response::from_string(body).with_header(hdr));
        return;
    }

    if url.starts_with("/api/chat") {
        let mut buf = Vec::new();
        let _ = request.as_reader().take(10 * 1024 * 1024).read_to_end(&mut buf);
        let (sid, msg, model_override, agent_sel) = parse_chat_body(&buf);
        if msg.is_empty() {
            let _ = request.respond(Response::from_string("missing msg").with_status_code(400));
            return;
        }
        if let Some(m) = &model_override {
            if let Err(e) = validate_model_override(m) {
                let body = format!("data: {}\n\n", json!({"type":"error","message": e}));
                let _ = request.respond(sse_response(mpsc_once(body)));
                return;
            }
        }
        let model = model_override.clone().or_else(|| cfg.model.clone());
        let agent_dir = resolve_agent(&cfg, &agent_sel);

        // Get-or-create the session for this conversation, then start a turn.
        // Keyed by (conversation, model): switching model starts a fresh model
        // context while the UI transcript stays visible. `send` spawns onto the
        // Tokio runtime, so enter its context first (plain std thread here).
        let key = format!("{sid}|{}|{agent_sel}", model.clone().unwrap_or_default());
        let turn = {
            let _enter = handle.enter();
            let mut map = sessions.lock().unwrap_or_else(|e| e.into_inner());
            if !map.contains_key(&key) {
                match open_session(agent_dir.clone(), model.clone(), None, cfg.permission_mode.clone()) {
                    Ok(s) => {
                        map.insert(key.clone(), s);
                    }
                    Err(e) => {
                        let body = format!("data: {}\n\n", json!({"type":"error","message": e.to_string()}));
                        let _ = request.respond(sse_response(mpsc_once(body)));
                        return;
                    }
                }
            }
            map.get(&key).unwrap().send(msg)
        };

        // Drive the turn on the async runtime, pushing SSE lines to the reader.
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        handle.spawn(async move {
            // 2KB comment padding up front: forces the browser to flush its
            // response buffer and START rendering immediately (otherwise Chrome
            // may hold the whole SSE stream and deliver it in one burst).
            let _ = tx.send(format!(":{}\n\n", " ".repeat(2048)).into_bytes());
            let mut turn = turn;
            while let Some(ev) = turn.next().await {
                if let Some(line) = sse_line(ev) {
                    if tx.send(line).is_err() {
                        return;
                    }
                }
            }
            let _ = tx.send(format!("data: {}\n\n", json!({"type":"done"})).into_bytes());
        });
        let _ = request.respond(sse_response(rx));
        return;
    }

    let _ = request.respond(Response::from_string("not found").with_status_code(404));
}

fn sse_line(ev: Event) -> Option<Vec<u8>> {
    let v = match ev {
        Event::Delta(t) => json!({ "type": "delta", "text": t }),
        Event::Thinking(t) => json!({ "type": "thinking", "text": t }),
        Event::ToolCall { name, args } => json!({ "type": "tool", "name": name, "args": args.to_string() }),
        Event::ToolResult { name, content, is_error } => {
            let preview: String = content.chars().take(200).collect();
            json!({ "type": "toolresult", "name": name, "error": is_error, "content": preview })
        }
        Event::Done => return None, // a final done is emitted after the loop
        Event::Error(e) => json!({ "type": "error", "message": e }),
    };
    Some(format!("data: {v}\n\n").into_bytes())
}

// ── SSE plumbing ────────────────────────────────────────────────────────────

/// A `Read` that yields bytes from a channel — EOF when the channel closes.
struct ChannelReader {
    rx: mpsc::Receiver<Vec<u8>>,
    buf: Vec<u8>,
    pos: usize,
}

impl Read for ChannelReader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if self.pos >= self.buf.len() {
            match self.rx.recv() {
                Ok(data) => {
                    self.buf = data;
                    self.pos = 0;
                }
                Err(_) => return Ok(0), // sender dropped → end of stream
            }
        }
        let n = (self.buf.len() - self.pos).min(out.len());
        out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

fn sse_response(rx: mpsc::Receiver<Vec<u8>>) -> Response<ChannelReader> {
    let headers = vec![
        Header::from_bytes(&b"Content-Type"[..], &b"text/event-stream"[..]).unwrap(),
        Header::from_bytes(&b"Cache-Control"[..], &b"no-cache"[..]).unwrap(),
        // Stop the browser from MIME-sniff-buffering the stream before rendering.
        Header::from_bytes(&b"X-Content-Type-Options"[..], &b"nosniff"[..]).unwrap(),
    ];
    Response::new(StatusCode(200), headers, ChannelReader { rx, buf: Vec::new(), pos: 0 }, None, None)
}

fn mpsc_once(body: String) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    let _ = tx.send(body.into_bytes());
    rx
}

// ── helpers ─────────────────────────────────────────────────────────────────

// ── Agent home: launch agent + user-created agents in ~/.ira/agents ────────

fn agents_root() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    home.join(".ira").join("agents")
}

/// Resolve an agent by name: "" or the launch agent's name → launch dir;
/// otherwise `~/.ira/agents/<slug>` (falls back to the launch dir if missing).
fn resolve_agent(cfg: &Arc<Cfg>, name: &str) -> PathBuf {
    if name.is_empty() {
        return cfg.dir.clone();
    }
    if let Ok(l) = crate::sdk::loader::load_agent(&cfg.dir) {
        if l.manifest.name.eq_ignore_ascii_case(name) {
            return cfg.dir.clone();
        }
    }
    let candidate = agents_root().join(slugify(name));
    if candidate.join("agent.yaml").exists() {
        candidate
    } else {
        cfg.dir.clone()
    }
}

fn slugify(name: &str) -> String {
    let s: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    s.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-")
}

/// All known agents: the launch agent first, then ~/.ira/agents/*.
fn list_agents(cfg: &Arc<Cfg>) -> String {
    let mut out = Vec::new();
    if let Ok(l) = crate::sdk::loader::load_agent(&cfg.dir) {
        out.push(json!({ "name": l.manifest.name, "id": "", "model": l.manifest.model.preferred,
                         "description": l.manifest.description, "current": true }));
    }
    if let Ok(rd) = std::fs::read_dir(agents_root()) {
        let mut dirs: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.join("agent.yaml").exists()).collect();
        dirs.sort();
        for d in dirs {
            // Skip if it IS the launch dir.
            if d.canonicalize().ok() == cfg.dir.canonicalize().ok() {
                continue;
            }
            if let Ok(l) = crate::sdk::loader::load_agent(&d) {
                out.push(json!({ "name": l.manifest.name, "id": l.manifest.name, "model": l.manifest.model.preferred,
                                 "description": l.manifest.description, "current": false }));
            }
        }
    }
    json!({ "agents": out }).to_string()
}

/// The most recently modified installed Ollama model, if the server is up.
fn first_installed_model(handle: &tokio::runtime::Handle) -> Option<String> {
    handle.block_on(async {
        let v: serde_json::Value = reqwest::Client::new()
            .get("http://localhost:11434/api/tags")
            .timeout(std::time::Duration::from_secs(3))
            .send()
            .await
            .ok()?
            .json()
            .await
            .ok()?;
        v.get("models")?
            .as_array()?
            .first()?
            .get("name")?
            .as_str()
            .map(String::from)
    })
}

/// Create a new agent under ~/.ira/agents from the UI form.
fn create_agent(handle: &tokio::runtime::Handle, body: &[u8]) -> String {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(body) else {
        return json!({ "error": "invalid json" }).to_string();
    };
    let name = v.get("name").and_then(|s| s.as_str()).unwrap_or("").trim().to_string();
    if name.is_empty() {
        return json!({ "error": "name is required" }).to_string();
    }
    let slug = slugify(&name);
    if slug.is_empty() {
        return json!({ "error": "name must contain letters or digits" }).to_string();
    }
    let link = agents_root().join(&slug);
    if link.join("agent.yaml").exists() {
        return json!({ "error": format!("agent '{name}' already exists") }).to_string();
    }
    // Optional workspace folder (onboarding): the agent lives in the user's
    // chosen folder; a symlink under ~/.ira/agents registers it for listing.
    let workspace = v.get("dir").and_then(|s| s.as_str()).unwrap_or("").trim().to_string();
    let dir = if workspace.is_empty() {
        link.clone()
    } else {
        let w = PathBuf::from(&workspace);
        if !w.is_absolute() || !w.is_dir() {
            return json!({ "error": "workspace folder not found" }).to_string();
        }
        let empty = std::fs::read_dir(&w).map(|mut r| r.next().is_none()).unwrap_or(false);
        let target = if empty { w } else { w.join(&slug) };
        if target.join("agent.yaml").exists() {
            return json!({ "error": "that folder already contains an agent" }).to_string();
        }
        target
    };
    let description = v.get("description").and_then(|s| s.as_str()).unwrap_or("").trim().to_string();
    let model = v.get("model").and_then(|s| s.as_str()).unwrap_or("").trim().to_string();
    let model = if model.is_empty() {
        // Default to a model that is actually installed on this machine.
        match first_installed_model(handle) {
            Some(m) => format!("ollama:{m}@http://localhost:11434/v1"),
            None => "ollama:gemma4:e2b-it-qat@http://localhost:11434/v1".to_string(),
        }
    } else {
        if let Err(e) = validate_model_override(&model) {
            return json!({ "error": e }).to_string();
        }
        model
    };
    let soul = v.get("soul").and_then(|s| s.as_str()).unwrap_or("").trim().to_string();
    let tools: Vec<String> = v
        .get("tools")
        .and_then(|t| t.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_else(|| vec!["cli".into(), "read".into(), "write".into(), "edit".into(), "memory".into(), "pdf".into()]);

    if std::fs::create_dir_all(&dir).is_err() {
        return json!({ "error": "could not create agent directory" }).to_string();
    }
    // Built as a YAML value (never string-templated) so user input can't
    // inject keys.
    let mut doc = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
    yaml_set(&mut doc, &["spec_version"], "0.1.0".into());
    yaml_set(&mut doc, &["name"], name.clone().into());
    yaml_set(&mut doc, &["description"], description.into());
    yaml_set(&mut doc, &["model", "preferred"], model.into());
    yaml_set(&mut doc, &["model", "fallback"], serde_yaml::Value::Sequence(Vec::new()));
    yaml_set(&mut doc, &["model", "constraints", "max_tokens"], 8192u64.into());
    yaml_set(&mut doc, &["tools"], yaml_strings(&tools));
    yaml_set(&mut doc, &["runtime", "max_turns"], 20u64.into());
    let Ok(yaml) = serde_yaml::to_string(&doc) else {
        return json!({ "error": "could not serialize agent.yaml" }).to_string();
    };
    if std::fs::write(dir.join("agent.yaml"), yaml).is_err() {
        return json!({ "error": "could not write agent.yaml" }).to_string();
    }
    if !soul.is_empty() {
        let _ = std::fs::write(dir.join("SOUL.md"), soul);
    }
    let _ = std::fs::create_dir_all(dir.join("memory"));
    let _ = std::fs::write(dir.join("memory").join("MEMORY.md"), "# Memory\n");
    if dir != link {
        let _ = std::fs::create_dir_all(agents_root());
        let _ = std::fs::remove_file(&link);
        #[cfg(unix)]
        let _ = std::os::unix::fs::symlink(&dir, &link);
    }
    json!({ "ok": true, "name": name, "dir": dir.display().to_string() }).to_string()
}

/// Full detail for the Studio editor (manifest + SOUL).
fn agent_detail(cfg: &Arc<Cfg>, id: &str) -> String {
    let dir = resolve_agent(cfg, id);
    let Ok(l) = crate::sdk::loader::load_agent(&dir) else {
        return json!({ "error": "agent not found" }).to_string();
    };
    let soul = std::fs::read_to_string(dir.join("SOUL.md")).unwrap_or_default();
    json!({
        "id": id, "name": l.manifest.name, "description": l.manifest.description,
        "model": l.manifest.model.preferred,
        "tools": l.manifest.tools.clone().unwrap_or_default(),
        "max_turns": l.manifest.max_turns(),
        "soul": soul,
        "deletable": !id.is_empty(),
    })
    .to_string()
}

/// Update an agent's manifest + SOUL from the Studio editor. The launch agent
/// (id "") is editable too; only its files change, never its location.
fn update_agent(cfg: &Arc<Cfg>, body: &[u8]) -> String {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(body) else {
        return json!({ "error": "invalid json" }).to_string();
    };
    let id = v.get("id").and_then(|s| s.as_str()).unwrap_or("");
    let dir = resolve_agent(cfg, id);
    // The name is fixed (it's the API identifier) and is never edited here.
    if crate::sdk::loader::load_agent(&dir).is_err() {
        return json!({ "error": "agent not found" }).to_string();
    }
    // Edit agent.yaml in place as a YAML value: only the fields the editor
    // sends change; everything else (permissions, sandbox, hooks…) is kept.
    let path = dir.join("agent.yaml");
    let Some(mut doc) = std::fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_yaml::from_str::<serde_yaml::Value>(&raw).ok())
        .filter(|d| d.is_mapping())
    else {
        return json!({ "error": "could not read agent.yaml" }).to_string();
    };
    if let Some(d) = v.get("description").and_then(|s| s.as_str()) {
        yaml_set(&mut doc, &["description"], d.trim().to_string().into());
    }
    if let Some(m) = v.get("model").and_then(|s| s.as_str()) {
        let m = m.trim().to_string();
        if let Err(e) = validate_model_override(&m) {
            return json!({ "error": e }).to_string();
        }
        yaml_set(&mut doc, &["model", "preferred"], m.into());
    }
    if let Some(n) = v.get("max_turns").and_then(|n| n.as_u64()) {
        yaml_set(&mut doc, &["runtime", "max_turns"], n.into());
    }
    if let Some(arr) = v.get("tools").and_then(|t| t.as_array()) {
        let tools: Vec<String> = arr.iter().filter_map(|x| x.as_str().map(String::from)).collect();
        yaml_set(&mut doc, &["tools"], yaml_strings(&tools));
    }
    let Ok(yaml) = serde_yaml::to_string(&doc) else {
        return json!({ "error": "could not serialize agent.yaml" }).to_string();
    };
    if std::fs::write(&path, yaml).is_err() {
        return json!({ "error": "could not write agent.yaml" }).to_string();
    }
    if let Some(soul) = v.get("soul").and_then(|s| s.as_str()) {
        if soul.trim().is_empty() {
            let _ = std::fs::remove_file(dir.join("SOUL.md"));
        } else {
            let _ = std::fs::write(dir.join("SOUL.md"), soul);
        }
    }
    json!({ "ok": true }).to_string()
}

/// Set `path` (nested mapping keys) in a YAML document, creating intermediate
/// mappings (and replacing non-mapping intermediates) as needed.
fn yaml_set(doc: &mut serde_yaml::Value, path: &[&str], value: serde_yaml::Value) {
    let Some((last, parents)) = path.split_last() else { return };
    let mut cur = doc;
    for key in parents {
        if !cur.is_mapping() {
            *cur = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
        }
        let map = cur.as_mapping_mut().expect("mapping");
        let k = serde_yaml::Value::String((*key).to_string());
        if !map.get(&k).map(|v| v.is_mapping()).unwrap_or(false) {
            map.insert(k.clone(), serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
        }
        cur = map.get_mut(&k).expect("just inserted");
    }
    if !cur.is_mapping() {
        *cur = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
    }
    cur.as_mapping_mut()
        .expect("mapping")
        .insert(serde_yaml::Value::String((*last).to_string()), value);
}

fn yaml_strings(items: &[String]) -> serde_yaml::Value {
    serde_yaml::Value::Sequence(items.iter().map(|t| serde_yaml::Value::String(t.clone())).collect())
}

/// Delete a created agent (never the launch agent; jailed to ~/.ira/agents).
fn delete_agent(id: &str) -> String {
    if id.is_empty() {
        return json!({ "error": "the active workspace agent cannot be deleted" }).to_string();
    }
    let dir = agents_root().join(slugify(id));
    if !dir.join("agent.yaml").exists() {
        return json!({ "error": "agent not found" }).to_string();
    }
    // Workspace agents are symlinked here: deleting only unregisters them —
    // the user's actual folder is never touched.
    if std::fs::symlink_metadata(&dir).map(|m| m.file_type().is_symlink()).unwrap_or(false) {
        return match std::fs::remove_file(&dir) {
            Ok(()) => json!({ "ok": true, "unlinked": true }).to_string(),
            Err(e) => json!({ "error": e.to_string() }).to_string(),
        };
    }
    // Safety: only ever delete inside ~/.ira/agents.
    let Ok(canon) = dir.canonicalize() else {
        return json!({ "error": "agent not found" }).to_string();
    };
    if !canon.starts_with(agents_root()) {
        return json!({ "error": "refusing to delete outside the agents home" }).to_string();
    }
    match std::fs::remove_dir_all(&canon) {
        Ok(()) => json!({ "ok": true }).to_string(),
        Err(e) => json!({ "error": e.to_string() }).to_string(),
    }
}

/// Drive one agent turn and answer in OpenAI chat-completion format
/// (streaming SSE chunks or a single JSON body).
fn serve_openai_completion(
    request: tiny_http::Request,
    dir: PathBuf,
    agent: String,
    prompt: String,
    stream: bool,
    handle: tokio::runtime::Handle,
) {
    let id = format!("chatcmpl-{:x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
    let created = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);

    let session = {
        let _enter = handle.enter();
        match open_session(dir, None, None, None) {
            Ok(s) => s,
            Err(e) => {
                let _ = request.respond(
                    Response::from_string(json!({ "error": e.to_string() }).to_string()).with_status_code(500),
                );
                return;
            }
        }
    };
    let turn = {
        let _enter = handle.enter();
        session.send(prompt)
    };

    if stream {
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        let (aid, amodel) = (id.clone(), agent.clone());
        handle.spawn(async move {
            let _session = session; // keep alive for the turn
            let mut turn = turn;
            let chunk = |delta: serde_json::Value, fin: Option<&str>| {
                format!(
                    "data: {}\n\n",
                    json!({ "id": aid, "object": "chat.completion.chunk", "created": created, "model": amodel,
                            "choices": [{ "index": 0, "delta": delta, "finish_reason": fin }] })
                )
            };
            let _ = tx.send(format!(":{}\n\n", " ".repeat(2048)).into_bytes());
            while let Some(ev) = turn.next().await {
                let line = match ev {
                    Event::Delta(t) => Some(chunk(json!({ "content": t }), None)),
                    Event::Thinking(t) => Some(chunk(json!({ "reasoning": t }), None)),
                    Event::Error(e) => Some(chunk(json!({ "content": format!("\n[error: {e}]") }), None)),
                    _ => None,
                };
                if let Some(l) = line {
                    if tx.send(l.into_bytes()).is_err() {
                        return;
                    }
                }
            }
            let _ = tx.send(chunk(json!({}), Some("stop")).into_bytes());
            let _ = tx.send(b"data: [DONE]\n\n".to_vec());
        });
        let _ = request.respond(sse_response(rx));
    } else {
        let (text, thinking) = handle.block_on(async {
            let _session = session;
            let mut turn = turn;
            let (mut text, mut think) = (String::new(), String::new());
            while let Some(ev) = turn.next().await {
                match ev {
                    Event::Delta(t) => text.push_str(&t),
                    Event::Thinking(t) => think.push_str(&t),
                    Event::Error(e) => text.push_str(&format!("\n[error: {e}]")),
                    _ => {}
                }
            }
            (text, think)
        });
        let mut message = json!({ "role": "assistant", "content": text });
        if !thinking.is_empty() {
            message["reasoning"] = json!(thinking);
        }
        let body = json!({ "id": id, "object": "chat.completion", "created": created, "model": agent,
                           "choices": [{ "index": 0, "message": message, "finish_reason": "stop" }] });
        let hdr = Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap();
        let _ = request.respond(Response::from_string(body.to_string()).with_header(hdr));
    }
}

// ── Studio skills (same backend chain as agent-studio-ui) ──────────────────

fn studio_urls() -> (Vec<String>, String) {
    // Prod first (matches the prod Memberstack app), dev as fallback.
    let pagos = match std::env::var("IRA_PAGOS_URL") {
        Ok(u) => vec![u],
        Err(_) => vec![
            "https://pagos-prod.studio.lyzr.ai/api/v1/".to_string(),
            "https://pagos-dev.test.studio.lyzr.ai/api/v1/".to_string(),
        ],
    };
    let skills = std::env::var("IRA_SKILLS_URL")
        .unwrap_or_else(|_| "https://skills-api.studio.lyzr.ai".into());
    (pagos, skills)
}

/// Cache of memberstack-token → studio api key (avoids re-running the chain).
fn studio_keys() -> &'static Mutex<HashMap<String, String>> {
    static K: std::sync::OnceLock<Mutex<HashMap<String, String>>> = std::sync::OnceLock::new();
    K.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Memberstack token → org → api key (the exact chain the studio frontend runs).
async fn studio_api_key(token: &str) -> Result<String, String> {
    if token.is_empty() {
        return Err("sign in first — the skills catalog needs your Lyzr session".into());
    }
    if let Some(k) = studio_keys().lock().unwrap_or_else(|e| e.into_inner()).get(token) {
        return Ok(k.clone());
    }
    let (pagos_bases, _) = studio_urls();
    let client = reqwest::Client::new();
    let mut last_err = String::new();

    for pagos in &pagos_bases {
        // 1) current organization
        let org: serde_json::Value = match client
            .get(format!("{pagos}organizations/current"))
            .bearer_auth(token)
            .timeout(std::time::Duration::from_secs(15))
            .send()
            .await
        {
            Ok(r) => r.json().await.unwrap_or(serde_json::Value::Null),
            Err(e) => {
                last_err = e.to_string();
                continue;
            }
        };
        let Some(org_id) = org
            .pointer("/current_organization/_id")
            .or_else(|| org.pointer("/data/current_organization/_id"))
            .and_then(|v| v.as_str())
            .map(String::from)
        else {
            last_err = format!(
                "no organization ({})",
                org.to_string().chars().take(120).collect::<String>()
            );
            continue; // try the next pagos environment
        };

        // 2) org api keys
        let keys: serde_json::Value = match client
            .get(format!("{pagos}keys/by_organization"))
            .query(&[("organization_id", org_id.as_str())])
            .bearer_auth(token)
            .timeout(std::time::Duration::from_secs(15))
            .send()
            .await
        {
            Ok(r) => r.json().await.unwrap_or(serde_json::Value::Null),
            Err(e) => {
                last_err = e.to_string();
                continue;
            }
        };
        let Some(api_key) = keys
            .as_array()
            .and_then(|a| a.first())
            .or_else(|| keys.pointer("/data/0"))
            .and_then(|k| k.get("api_key"))
            .and_then(|v| v.as_str())
            .map(String::from)
        else {
            last_err = "no api key on this organization".to_string();
            continue;
        };

        studio_keys().lock().unwrap_or_else(|e| e.into_inner()).insert(token.to_string(), api_key.clone());
        return Ok(api_key);
    }
    Err(last_err)
}

/// GET /v1/skills from the studio skills service.
async fn studio_list_skills(token: &str, search: &str) -> String {
    let api_key = match studio_api_key(token).await {
        Ok(k) => k,
        Err(e) => return json!({ "error": e }).to_string(),
    };
    let (_, skills_url) = studio_urls();
    let mut req = reqwest::Client::new()
        .get(format!("{skills_url}/v1/skills"))
        .query(&[("limit", "100"), ("offset", "0")])
        .header("x-api-key", &api_key)
        .header("accept", "application/json")
        .timeout(std::time::Duration::from_secs(20));
    if !search.is_empty() {
        req = req.query(&[("search", search)]);
    }
    match req.send().await {
        Ok(r) if r.status().is_success() => r.text().await.unwrap_or_else(|_| "{}".into()),
        Ok(r) => json!({ "error": format!("skills service: HTTP {}", r.status()) }).to_string(),
        Err(e) => json!({ "error": e.to_string() }).to_string(),
    }
}

/// Install a studio skill into an agent: clone its repo into skills/<slug>.
fn install_studio_skill(cfg: &Arc<Cfg>, body: &[u8]) -> String {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(body) else {
        return json!({ "error": "invalid json" }).to_string();
    };
    let name = v.get("name").and_then(|s| s.as_str()).unwrap_or("");
    let repo = v.get("repository_url").and_then(|s| s.as_str()).unwrap_or("");
    let branch = v.get("default_branch").and_then(|s| s.as_str()).filter(|b| !b.is_empty());
    let agent = v.get("agent").and_then(|s| s.as_str()).unwrap_or("");
    if name.is_empty() || repo.is_empty() {
        return json!({ "error": "name and repository_url are required" }).to_string();
    }
    if !(repo.starts_with("https://") || repo.starts_with("http://")) {
        return json!({ "error": "only http(s) repositories are supported" }).to_string();
    }
    let subpath = v.get("external_repo_path").and_then(|s| s.as_str()).unwrap_or("").trim_matches('/');
    if !is_plain_relative(subpath) {
        return json!({ "error": "external_repo_path must be a relative path inside the repository" }).to_string();
    }
    let adir = resolve_agent(cfg, agent);
    let slug = slugify(name);
    let dest = adir.join("skills").join(&slug);
    if dest.exists() {
        return json!({ "error": format!("skills/{slug} already exists in this agent") }).to_string();
    }
    let _ = std::fs::create_dir_all(adir.join("skills"));

    // Clone into a temp dir first — many skills live in a shared monorepo, and
    // only the skill's own folder should land in the agent.
    let tmp = adir.join(".gitagent").join(format!("skill-tmp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    if !crate::sdk::loader::clone_git_repo(repo, &tmp, &adir, branch) {
        return json!({ "error": "clone failed — the skill repository may be private or unreachable" }).to_string();
    }
    let _ = std::fs::remove_dir_all(tmp.join(".git"));

    // Pick the skill's root inside the clone:
    //   1) the catalog's explicit subpath, 2) a subdir named like the skill
    //      that carries SKILL.md, 3) the repo root (single-skill repo).
    let mut src = tmp.clone();
    if !subpath.is_empty() && tmp.join(subpath).is_dir() {
        src = tmp.join(subpath);
    } else if !tmp.join("SKILL.md").exists() {
        let cand = ["skills", ""].iter().find_map(|base| {
            let root = if base.is_empty() { tmp.clone() } else { tmp.join(base) };
            let rd = std::fs::read_dir(&root).ok()?;
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() && p.join("SKILL.md").exists() {
                    let n = e.file_name().to_string_lossy().to_lowercase();
                    if n == slug || n == name.to_lowercase() || slugify(&n) == slug {
                        return Some(p);
                    }
                }
            }
            None
        });
        if let Some(c) = cand {
            src = c;
        }
    }

    // The chosen root must resolve (symlinks included) inside the clone.
    let confined = match (src.canonicalize(), tmp.canonicalize()) {
        (Ok(s), Ok(t)) => s.starts_with(&t),
        _ => false,
    };
    if !confined {
        let _ = std::fs::remove_dir_all(&tmp);
        return json!({ "error": "skill path resolves outside the repository" }).to_string();
    }
    if let Err(e) = copy_dir(&src, &dest) {
        let _ = std::fs::remove_dir_all(&tmp);
        return json!({ "error": format!("copy failed: {e}") }).to_string();
    }
    let _ = std::fs::remove_dir_all(&tmp);
    let files = std::fs::read_dir(&dest).map(|rd| rd.count()).unwrap_or(0);
    let scoped = src != tmp;

    // Remember the install in .gitagent/skills.json (the agent's skill ledger).
    let skill_id = v.get("skill_id").and_then(|s| s.as_str()).unwrap_or("");
    let log_path = adir.join(".gitagent").join("skills.json");
    let mut log: serde_json::Value = std::fs::read_to_string(&log_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| json!({ "skills": [] }));
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Some(arr) = log.get_mut("skills").and_then(|a| a.as_array_mut()) {
        arr.retain(|e| e.get("slug").and_then(|s| s.as_str()) != Some(slug.as_str()));
        arr.push(json!({ "skill_id": skill_id, "name": name, "slug": slug,
                          "path": format!("skills/{slug}"), "repository_url": repo,
                          "installed_at": ts }));
    }
    let _ = std::fs::create_dir_all(adir.join(".gitagent"));
    let _ = std::fs::write(&log_path, serde_json::to_string_pretty(&log).unwrap_or_default());

    json!({ "ok": true, "path": format!("skills/{slug}"), "files": files, "scoped": scoped }).to_string()
}

/// The agent's installed-skill ledger, reconciled against the filesystem:
/// stale entries (folder deleted) are dropped; untracked skills/ folders are
/// included so manual installs also count as "added".
fn installed_skills(adir: &std::path::Path) -> String {
    let log_path = adir.join(".gitagent").join("skills.json");
    let log: serde_json::Value = std::fs::read_to_string(&log_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| json!({ "skills": [] }));
    let mut entries: Vec<serde_json::Value> = log
        .get("skills")
        .and_then(|a| a.as_array())
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|e| {
            e.get("path")
                .and_then(|p| p.as_str())
                .map(|p| adir.join(p).is_dir())
                .unwrap_or(false)
        })
        .collect();
    // Untracked folders under skills/.
    let known: std::collections::HashSet<String> = entries
        .iter()
        .filter_map(|e| e.get("slug").and_then(|s| s.as_str()).map(String::from))
        .collect();
    if let Ok(rd) = std::fs::read_dir(adir.join("skills")) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if e.path().is_dir() && !known.contains(&name) {
                entries.push(json!({ "skill_id": "", "name": name, "slug": name,
                                      "path": format!("skills/{name}"), "untracked": true }));
            }
        }
    }
    json!({ "skills": entries }).to_string()
}

/// A non-empty-or-empty relative path made only of normal components (no
/// `..`, no root/prefix) — safe to join under a directory.
fn is_plain_relative(p: &str) -> bool {
    use std::path::Component;
    let path = std::path::Path::new(p);
    !path.is_absolute() && path.components().all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
}

/// Recursive copy (skips .git). Symlinks and special files are skipped, never
/// followed — a cloned repo must not be able to pull in files from elsewhere
/// on disk (or loop forever).
fn copy_dir(src: &std::path::Path, dest: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dest)?;
    for e in std::fs::read_dir(src)?.flatten() {
        let name = e.file_name();
        if name == ".git" {
            continue;
        }
        let from = e.path();
        let to = dest.join(&name);
        let ft = std::fs::symlink_metadata(&from)?.file_type();
        if ft.is_dir() {
            copy_dir(&from, &to)?;
        } else if ft.is_file() {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

// ── OAuth handoff state ─────────────────────────────────────────────────────

fn handoff() -> &'static Mutex<Option<String>> {
    static H: std::sync::OnceLock<Mutex<Option<String>>> = std::sync::OnceLock::new();
    H.get_or_init(|| Mutex::new(None))
}

fn server_base_cell() -> &'static Mutex<Option<String>> {
    static B: std::sync::OnceLock<Mutex<Option<String>>> = std::sync::OnceLock::new();
    B.get_or_init(|| Mutex::new(None))
}
fn server_base() -> Option<String> {
    server_base_cell().lock().unwrap_or_else(|e| e.into_inner()).clone()
}

// ── Ollama engine auto-setup: fetch the official CLI, run `serve` ourselves ─

#[derive(Default, Clone)]
struct EngineState {
    phase: String, // idle | checking | downloading | installing | starting | ready | error
    total: u64,
    completed: u64,
    error: Option<String>,
}

fn engine() -> &'static Mutex<EngineState> {
    static E: std::sync::OnceLock<Mutex<EngineState>> = std::sync::OnceLock::new();
    E.get_or_init(|| Mutex::new(EngineState { phase: "idle".into(), ..Default::default() }))
}

fn ira_home() -> PathBuf {
    agents_root().parent().map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from("."))
}

/// Any usable ollama binary already on this machine (ours last-installed wins).
fn find_ollama_bin() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    [
        ira_home().join("bin").join("ollama"),
        PathBuf::from("/opt/homebrew/bin/ollama"),
        PathBuf::from("/usr/local/bin/ollama"),
        PathBuf::from("/Applications/Ollama.app/Contents/Resources/ollama"),
        home.join("Applications/Ollama.app/Contents/Resources/ollama"),
    ]
    .iter()
    .find(|p| p.is_file())
    .cloned()
}

async fn ollama_api_up() -> bool {
    reqwest::Client::new()
        .get("http://localhost:11434/api/version")
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

fn spawn_serve(bin: &std::path::Path) -> std::io::Result<()> {
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(ira_home().join("ollama.log"))?;
    let err = log.try_clone()?;
    std::process::Command::new(bin)
        .arg("serve")
        .stdin(std::process::Stdio::null())
        .stdout(log)
        .stderr(err)
        .spawn()?;
    Ok(())
}

/// Kick the engine setup unless one is already in flight.
fn start_engine_setup(handle: &tokio::runtime::Handle) {
    {
        let mut st = engine().lock().unwrap_or_else(|e| e.into_inner());
        if matches!(st.phase.as_str(), "checking" | "downloading" | "installing" | "starting") {
            return;
        }
        *st = EngineState { phase: "checking".into(), ..Default::default() };
    }
    handle.spawn(async move {
        let set = |f: &dyn Fn(&mut EngineState)| {
            let mut st = engine().lock().unwrap_or_else(|e| e.into_inner());
            f(&mut st);
        };
        let fail = |msg: String| {
            let mut st = engine().lock().unwrap_or_else(|e| e.into_inner());
            st.phase = "error".into();
            st.error = Some(msg);
        };
        if ollama_api_up().await {
            return set(&|st| st.phase = "ready".into());
        }
        let bin = match find_ollama_bin() {
            Some(b) => b,
            None => {
                // Download the official CLI release (universal macOS binary).
                set(&|st| st.phase = "downloading".into());
                let dir = ira_home().join("bin");
                if let Err(e) = std::fs::create_dir_all(&dir) {
                    return fail(format!("could not create {}: {e}", dir.display()));
                }
                let tgz = dir.join("ollama-darwin.tgz");
                let resp = reqwest::Client::new()
                    .get("https://github.com/ollama/ollama/releases/latest/download/ollama-darwin.tgz")
                    .timeout(std::time::Duration::from_secs(1800))
                    .send()
                    .await;
                let mut resp = match resp {
                    Ok(r) if r.status().is_success() => r,
                    Ok(r) => return fail(format!("engine download failed: HTTP {}", r.status())),
                    Err(e) => return fail(format!("engine download failed: {e}")),
                };
                let total = resp.content_length().unwrap_or(0);
                set(&|st| st.total = total);
                let mut file = match std::fs::File::create(&tgz) {
                    Ok(f) => f,
                    Err(e) => return fail(format!("could not write download: {e}")),
                };
                let mut done: u64 = 0;
                loop {
                    match resp.chunk().await {
                        Ok(Some(bytes)) => {
                            use std::io::Write;
                            if let Err(e) = file.write_all(&bytes) {
                                return fail(format!("write failed: {e}"));
                            }
                            done += bytes.len() as u64;
                            set(&|st| st.completed = done);
                        }
                        Ok(None) => break,
                        Err(e) => return fail(format!("engine download interrupted: {e}")),
                    }
                }
                drop(file);
                set(&|st| st.phase = "installing".into());
                let out = std::process::Command::new("/usr/bin/tar")
                    .arg("-xzf")
                    .arg(&tgz)
                    .arg("-C")
                    .arg(&dir)
                    .output();
                let _ = std::fs::remove_file(&tgz);
                match out {
                    Ok(o) if o.status.success() => {}
                    Ok(o) => return fail(format!("unpack failed: {}", String::from_utf8_lossy(&o.stderr))),
                    Err(e) => return fail(format!("unpack failed: {e}")),
                }
                let b = dir.join("ollama");
                if !b.is_file() {
                    return fail("unpack produced no ollama binary".into());
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(&b, std::fs::Permissions::from_mode(0o755));
                }
                b
            }
        };
        set(&|st| st.phase = "starting".into());
        if let Err(e) = spawn_serve(&bin) {
            return fail(format!("could not start the engine: {e}"));
        }
        for _ in 0..60 {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            if ollama_api_up().await {
                return set(&|st| st.phase = "ready".into());
            }
        }
        fail("engine did not come up — see ~/.ira/ollama.log".into())
    });
}

// ── Ollama pull manager: server-owned downloads that survive page reloads ──

#[derive(Default, Clone)]
struct PullState {
    status: String,
    total: u64,
    completed: u64,
    done: bool,
    error: Option<String>,
    cancel: bool,
}

fn pulls() -> &'static Mutex<HashMap<String, PullState>> {
    static P: std::sync::OnceLock<Mutex<HashMap<String, PullState>>> = std::sync::OnceLock::new();
    P.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Start (or no-op if already running) a background pull of an Ollama model.
fn start_pull(handle: &tokio::runtime::Handle, name: &str) {
    {
        let mut map = pulls().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(st) = map.get(name) {
            if !st.done && st.error.is_none() {
                return; // already in flight
            }
        }
        map.insert(name.to_string(), PullState { status: "starting".into(), ..Default::default() });
    }
    let name = name.to_string();
    handle.spawn(async move {
        let set = |f: &dyn Fn(&mut PullState)| {
            let mut map = pulls().lock().unwrap_or_else(|e| e.into_inner());
            if let Some(st) = map.get_mut(&name) {
                f(st);
            }
        };
        let resp = reqwest::Client::new()
            .post("http://localhost:11434/api/pull")
            .json(&json!({ "name": name, "stream": true }))
            .send()
            .await;
        let mut resp = match resp {
            Ok(r) if r.status().is_success() => r,
            Ok(r) => return set(&|st| st.error = Some(format!("HTTP {}", r.status()))),
            Err(e) => return set(&|st| st.error = Some(e.to_string())),
        };
        let mut buf = String::new();
        loop {
            // A cancelled (or removed) pull stops here; dropping `resp` closes
            // the stream and Ollama keeps resumable partial blobs.
            let cancelled = {
                let map = pulls().lock().unwrap_or_else(|e| e.into_inner());
                map.get(&name).map(|st| st.cancel).unwrap_or(true)
            };
            if cancelled {
                let mut map = pulls().lock().unwrap_or_else(|e| e.into_inner());
                map.remove(&name);
                return;
            }
            match resp.chunk().await {
                Ok(Some(bytes)) => {
                    buf.push_str(&String::from_utf8_lossy(&bytes));
                    while let Some(nl) = buf.find('\n') {
                        let line: String = buf.drain(..=nl).collect();
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim()) {
                            set(&|st| {
                                if let Some(s) = v.get("status").and_then(|s| s.as_str()) {
                                    st.status = s.to_string();
                                }
                                if let Some(t) = v.get("total").and_then(|t| t.as_u64()) {
                                    st.total = t;
                                }
                                if let Some(c) = v.get("completed").and_then(|c| c.as_u64()) {
                                    st.completed = c;
                                }
                                if let Some(e) = v.get("error").and_then(|e| e.as_str()) {
                                    st.error = Some(e.to_string());
                                }
                            });
                        }
                    }
                }
                Ok(None) => break,
                Err(e) => return set(&|st| st.error = Some(e.to_string())),
            }
        }
        set(&|st| {
            if st.error.is_none() {
                st.done = true;
                st.status = "success".into();
            }
        });
    });
}

/// Save an uploaded file into `<agent-dir>/uploads/` (sanitized name, deduped,
/// 100 MB cap). Returns JSON {path, size} or {error}.
fn save_upload(root: &std::path::Path, name: &str, request: &mut tiny_http::Request) -> String {
    // Basename only; drop anything path-like or shell-hostile.
    let base = name.rsplit(['/', '\\']).next().unwrap_or("file");
    let safe: String = base
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || ".-_ ".contains(c) { c } else { '_' })
        .collect();
    let safe = safe.trim().trim_start_matches('.').to_string();
    let safe = if safe.is_empty() { "file".to_string() } else { safe };

    let mut buf = Vec::new();
    let mut limited = request.as_reader().take(100 * 1024 * 1024 + 1);
    if limited.read_to_end(&mut buf).is_err() {
        return json!({ "error": "read failed" }).to_string();
    }
    if buf.len() > 100 * 1024 * 1024 {
        return json!({ "error": "file exceeds the 100 MB limit" }).to_string();
    }

    let uploads = root.join("uploads");
    if std::fs::create_dir_all(&uploads).is_err() {
        return json!({ "error": "cannot create uploads dir" }).to_string();
    }
    // Dedupe: name.ext, name-1.ext, name-2.ext…
    let (stem, ext) = match safe.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (safe.clone(), String::new()),
    };
    let mut target = uploads.join(&safe);
    let mut n = 0;
    while target.exists() {
        n += 1;
        target = uploads.join(format!("{stem}-{n}{ext}"));
    }
    let size = buf.len();
    match std::fs::write(&target, buf) {
        Ok(()) => {
            let rel = format!("uploads/{}", target.file_name().unwrap_or_default().to_string_lossy());
            json!({ "path": rel, "size": size }).to_string()
        }
        Err(e) => json!({ "error": e.to_string() }).to_string(),
    }
}

/// Write a text file inside the agent dir (editor save). Creates parent dirs so
/// new spec files (skills/x/SKILL.md, knowledge/…) can be added from the editor.
fn save_text_file(root: &std::path::Path, rel: &str, bytes: &[u8]) -> String {
    // Jail without requiring the file to exist yet: sanitize each component.
    let rel = rel.trim_start_matches('/');
    if rel.is_empty() || rel.split('/').any(|c| c == ".." || c.is_empty() || c.starts_with('.') && c != ".gitagent") {
        return json!({ "error": "invalid path" }).to_string();
    }
    if std::str::from_utf8(bytes).is_err() {
        return json!({ "error": "editor saves text files only" }).to_string();
    }
    let target = root.join(rel);
    if let Some(parent) = target.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::write(&target, bytes) {
        Ok(()) => json!({ "ok": true, "path": rel, "size": bytes.len() }).to_string(),
        Err(e) => json!({ "error": e.to_string() }).to_string(),
    }
}

/// Resolve a browser-supplied relative path INSIDE the agent dir (traversal-safe).
fn safe_join(root: &std::path::Path, rel: &str) -> Option<PathBuf> {
    let joined = root.join(rel.trim_start_matches('/'));
    let canon = joined.canonicalize().ok()?;
    let rootc = root.canonicalize().ok()?;
    canon.starts_with(&rootc).then_some(canon)
}

/// One directory level of the agent workspace as JSON (dirs first, dotfiles
/// hidden except .gitagent; .env and .git never listed — they hold secrets/noise).
/// Top-level entries that belong to the open agent spec (the editor view).
const SPEC_FILES: &[&str] = &["agent.yaml", "SOUL.md", "RULES.md", "DUTIES.md", "AGENTS.md", "hooks.yaml", "mcp.yaml", "README.md"];
const SPEC_DIRS: &[&str] = &["skills", "knowledge", "workflows", "examples", "tools", "agents", "memory", "config"];

fn list_files(root: &std::path::Path, rel: &str, spec_only: bool) -> String {
    let Some(dir) = safe_join(root, rel).filter(|p| p.is_dir()) else {
        return json!({ "entries": [] }).to_string();
    };
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name == ".git" || name.starts_with(".env") || (name.starts_with('.') && name != ".gitagent") {
                continue;
            }
            let is_dir = e.path().is_dir();
            // Editor view: only agent-spec entries at the top level (workspace
            // outputs like uploads/, generated files, .gitagent live elsewhere).
            if spec_only && rel.is_empty() {
                let allowed = if is_dir { SPEC_DIRS.contains(&name.as_str()) } else { SPEC_FILES.contains(&name.as_str()) };
                if !allowed {
                    continue;
                }
            }
            let rel_path = if rel.is_empty() { name.clone() } else { format!("{}/{}", rel.trim_end_matches('/'), name) };
            let size = e.metadata().map(|m| m.len()).unwrap_or(0);
            let item = json!({ "name": name, "path": rel_path, "dir": is_dir, "size": size });
            if is_dir { dirs.push(item) } else { files.push(item) }
        }
    }
    let key = |v: &serde_json::Value| v["name"].as_str().unwrap_or("").to_lowercase();
    dirs.sort_by_key(key);
    files.sort_by_key(key);
    dirs.extend(files);
    json!({ "entries": dirs }).to_string()
}

/// Classify + load a file for the in-UI viewer: text (content inline), image,
/// pdf, or binary (open externally). Text is capped at 2 MB.
fn file_meta(root: &std::path::Path, rel: &str) -> String {
    let Some(p) = safe_join(root, rel).filter(|p| p.is_file()) else {
        return json!({ "kind": "missing" }).to_string();
    };
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    if ["png", "jpg", "jpeg", "gif", "webp", "svg", "ico", "bmp"].contains(&ext.as_str()) {
        return json!({ "kind": "image" }).to_string();
    }
    if ext == "pdf" {
        return json!({ "kind": "pdf" }).to_string();
    }
    // Too big to load into memory for the viewer → open externally.
    if std::fs::metadata(&p).map(|m| m.len() > 32 * 1024 * 1024).unwrap_or(true) {
        return json!({ "kind": "binary" }).to_string();
    }
    // Documents: show extracted text in the viewer tab.
    if crate::sdk::extract::is_document(&p) {
        if let Ok(bytes) = std::fs::read(&p) {
            if let Some(text) = crate::sdk::extract::extract_text(&p, &bytes) {
                return json!({ "kind": "text", "content": text, "ext": "txt" }).to_string();
            }
        }
        return json!({ "kind": "binary" }).to_string();
    }
    match std::fs::read(&p) {
        Ok(bytes) if bytes.len() <= 2_000_000 && !bytes.contains(&0) => match String::from_utf8(bytes) {
            Ok(content) => json!({ "kind": "text", "content": content, "ext": ext }).to_string(),
            Err(_) => json!({ "kind": "binary" }).to_string(),
        },
        Ok(_) => json!({ "kind": "binary" }).to_string(),
        Err(_) => json!({ "kind": "missing" }).to_string(),
    }
}

fn content_type_for(p: &std::path::Path) -> String {
    match p.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase().as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "pdf" => "application/pdf",
        "html" => "text/html; charset=utf-8",
        "txt" | "md" | "yaml" | "yml" | "json" | "rs" | "py" | "js" | "ts" | "toml" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
    .to_string()
}

/// Extract one query parameter (URL-decoded) from a request URL.
fn query_param(url: &str, key: &str) -> Option<String> {
    let query = url.splitn(2, '?').nth(1)?;
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            if k == key {
                return Some(url_decode(v));
            }
        }
    }
    None
}

/// The models the UI can offer: the agent's default plus every local Ollama tag.
async fn list_models(cfg: &Arc<Cfg>) -> String {
    let mut out: Vec<serde_json::Value> = Vec::new();
    // Agent default first (empty id = "use the agent's configured model").
    let default_label = crate::sdk::loader::load_agent(&cfg.dir)
        .map(|l| l.manifest.model.preferred)
        .unwrap_or_else(|_| "agent default".into());
    out.push(json!({ "id": "", "label": format!("Default · {}", short_model(&default_label)) }));
    // Local Ollama models.
    if let Ok(resp) = reqwest::Client::new()
        .get("http://localhost:11434/api/tags")
        .timeout(std::time::Duration::from_secs(3))
        .send()
        .await
    {
        if let Ok(v) = resp.json::<serde_json::Value>().await {
            for m in v.get("models").and_then(|m| m.as_array()).unwrap_or(&Vec::new()) {
                if let Some(name) = m.get("model").and_then(|n| n.as_str()) {
                    out.push(json!({
                        "id": format!("ollama:{name}@http://localhost:11434/v1"),
                        "label": friendly_model(name),
                    }));
                }
            }
        }
    }
    json!({ "models": out }).to_string()
}

/// Human label for an Ollama tag ("gemma4:e2b-it-qat" → "Gemma 4 · e2b-it-qat").
fn friendly_model(tag: &str) -> String {
    let lower = tag.to_lowercase();
    let family = if lower.contains("muse-glimmer") || lower.contains("muse_glimmer") {
        "Muse Glimmer"
    } else if lower.starts_with("gemma4") {
        "Gemma 4"
    } else if lower.starts_with("gemma3") {
        "Gemma 3"
    } else if lower.contains("qwen") {
        "Qwen"
    } else if lower.contains("llama") {
        "Llama"
    } else {
        return tag.to_string();
    };
    match tag.split_once(':') {
        Some((_, variant)) => format!("{family} · {variant}"),
        None => family.to_string(),
    }
}

/// Compact display of a model spec string (strip provider + base URL).
fn short_model(spec: &str) -> String {
    let s = spec.split_once(':').map(|(_, r)| r).unwrap_or(spec);
    s.split('@').next().unwrap_or(s).to_string()
}

/// Parse the chat request body `{session, msg, model, agent}`.
fn parse_chat_body(body: &[u8]) -> (String, String, Option<String>, String) {
    let v: serde_json::Value = serde_json::from_slice(body).unwrap_or(serde_json::Value::Null);
    let field = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    let sid = Some(field("session")).filter(|s| !s.is_empty()).unwrap_or_else(|| "default".to_string());
    let model = Some(field("model")).filter(|m| !m.is_empty());
    (sid, field("msg"), model, field("agent"))
}

/// Minimal percent-decoding (also `+` → space).
fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok());
                match hex {
                    Some(b) => {
                        out.push(b);
                        i += 3;
                    }
                    None => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
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

/// Preload a LOCAL model (Ollama keeps weights in RAM ~5 min) so the first
/// message doesn't pay the multi-second cold-load. Local endpoints only —
/// never spends cloud tokens. Fire-and-forget.
fn warm_model(cfg: &Arc<Cfg>) {
    let Ok(loaded) = crate::sdk::loader::load_agent(&cfg.dir) else { return };
    let model = cfg.model.clone().unwrap_or(loaded.manifest.model.preferred);
    let spec = crate::pi::provider::resolve_model(&model);
    if !(spec.base_url.contains("localhost") || spec.base_url.contains("127.0.0.1")) {
        return;
    }
    tokio::spawn(async move {
        let body = serde_json::json!({
            "model": spec.model_id,
            "messages": [{"role": "user", "content": "hi"}],
            "max_tokens": 1, "stream": false
        });
        let url = format!("{}/chat/completions", spec.base_url.trim_end_matches('/'));
        let _ = reqwest::Client::new().post(&url).json(&body).send().await;
    });
}

/// Kill stale `ira` processes holding our port range (previous builds serve old
/// embedded UI/server code). Only processes whose command is `ira` are touched.
fn kill_stale_instances() {
    let me = std::process::id();
    for port in 8787u16..8797 {
        let Ok(out) = std::process::Command::new("lsof").args(["-ti", &format!(":{port}")]).output() else { continue };
        for pid in String::from_utf8_lossy(&out.stdout).split_whitespace() {
            let Ok(pid_n) = pid.parse::<u32>() else { continue };
            if pid_n == me {
                continue;
            }
            // Check the process name before killing.
            let Ok(ps) = std::process::Command::new("ps").args(["-p", pid, "-o", "comm="]).output() else { continue };
            let name = String::from_utf8_lossy(&ps.stdout).trim().to_string();
            if name.ends_with("/ira") || name == "ira" {
                let _ = std::process::Command::new("kill").arg(pid).output();
                eprintln!("(replaced stale ira instance on :{port})");
            }
        }
    }
    std::thread::sleep(std::time::Duration::from_millis(300));
}

fn open_browser(url: &str) {
    let cmd = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "explorer"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(cmd).arg(url).spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOK: &str = "0123456789abcdef0123456789abcdef";

    fn hdrs(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn authorize_checks_host_origin_token_and_method() {
        let ok_host = ("host", "127.0.0.1:8787");
        let tok = ("x-ira-token", TOK);
        let json = ("content-type", "application/json");
        // The page and vendored assets are public (but still Host-checked).
        assert!(authorize("GET", "/", &hdrs(&[ok_host]), 8787, TOK).is_ok());
        assert!(authorize("GET", "/vendor/cm.js", &hdrs(&[("host", "localhost:8787")]), 8787, TOK).is_ok());
        assert_eq!(authorize("GET", "/", &hdrs(&[("host", "evil.example:8787")]), 8787, TOK).unwrap_err().0, 403);
        assert_eq!(authorize("GET", "/", &hdrs(&[]), 8787, TOK).unwrap_err().0, 403);
        assert_eq!(authorize("GET", "/", &hdrs(&[("host", "127.0.0.1:9999")]), 8787, TOK).unwrap_err().0, 403);
        // API calls need the token.
        assert_eq!(authorize("GET", "/api/agents", &hdrs(&[ok_host]), 8787, TOK).unwrap_err().0, 401);
        assert_eq!(authorize("GET", "/api/agents", &hdrs(&[ok_host, ("x-ira-token", "nope")]), 8787, TOK).unwrap_err().0, 401);
        assert!(authorize("GET", "/api/agents", &hdrs(&[ok_host, tok]), 8787, TOK).is_ok());
        let bearer = format!("Bearer {TOK}");
        assert!(authorize("POST", "/v1/chat/completions", &hdrs(&[ok_host, ("authorization", &bearer), json]), 8787, TOK).is_ok());
        // Query-string token only for /raw GETs.
        assert!(authorize("GET", &format!("/raw?path=a.png&token={TOK}"), &hdrs(&[ok_host]), 8787, TOK).is_ok());
        assert_eq!(authorize("GET", &format!("/api/files?token={TOK}"), &hdrs(&[ok_host]), 8787, TOK).unwrap_err().0, 401);
        // Foreign Origin is rejected even with the token.
        assert_eq!(
            authorize("GET", "/api/agents", &hdrs(&[ok_host, tok, ("origin", "https://evil.example")]), 8787, TOK).unwrap_err().0,
            403
        );
        assert_eq!(authorize("GET", "/api/agents", &hdrs(&[ok_host, tok, ("origin", "null")]), 8787, TOK).unwrap_err().0, 403);
        assert!(authorize("GET", "/api/agents", &hdrs(&[ok_host, tok, ("origin", "http://127.0.0.1:8787")]), 8787, TOK).is_ok());
        // Chat is POST + JSON only.
        assert_eq!(authorize("GET", "/api/chat?msg=hi", &hdrs(&[ok_host, tok]), 8787, TOK).unwrap_err().0, 405);
        assert_eq!(authorize("POST", "/api/chat", &hdrs(&[ok_host, tok, ("content-type", "text/plain")]), 8787, TOK).unwrap_err().0, 415);
        assert!(authorize("POST", "/api/chat", &hdrs(&[ok_host, tok, json]), 8787, TOK).is_ok());
        // Process-launching / destructive endpoints are POST only.
        assert_eq!(authorize("GET", "/api/ollama/delete?name=x", &hdrs(&[ok_host, tok]), 8787, TOK).unwrap_err().0, 405);
        assert_eq!(authorize("GET", "/api/open?path=x", &hdrs(&[ok_host, tok]), 8787, TOK).unwrap_err().0, 405);
        assert!(authorize("POST", "/api/ollama/pull?name=x", &hdrs(&[ok_host, tok]), 8787, TOK).is_ok());
        // Polling endpoints stay GET.
        assert!(authorize("GET", "/api/ollama/pulls", &hdrs(&[ok_host, tok]), 8787, TOK).is_ok());
        assert!(authorize("GET", "/api/auth/handoff", &hdrs(&[ok_host, tok]), 8787, TOK).is_ok());
        assert_eq!(authorize("POST", "/api/auth/handoff", &hdrs(&[ok_host, tok]), 8787, TOK).unwrap_err().0, 415);
    }

    #[test]
    fn launch_tokens_are_random_hex() {
        std::env::remove_var("IRA_UI_TOKEN");
        let (a, b) = (launch_token().unwrap(), launch_token().unwrap());
        assert_eq!(a.len(), 64);
        assert_ne!(a, b);
        assert!(ct_eq(&a, &a.clone()) && !ct_eq(&a, &b) && !ct_eq(&a, "short"));
    }

    #[test]
    fn model_overrides_from_requests_must_be_local() {
        assert!(validate_model_override("openai:gpt-4o").is_ok());
        assert!(validate_model_override("ollama:gemma3:4b@http://localhost:11434/v1").is_ok());
        assert!(validate_model_override("openai:gpt-4o@https://evil.example/v1").is_err());
        assert!(validate_model_override("ollama:x@http://localhost.evil.example/v1").is_err());
    }

    #[test]
    fn chat_body_parses() {
        let (sid, msg, model, agent) = parse_chat_body(br#"{"session":"s1","msg":"hi","model":"","agent":"a"}"#);
        assert_eq!((sid.as_str(), msg.as_str(), model, agent.as_str()), ("s1", "hi", None, "a"));
        let (sid, msg, _, _) = parse_chat_body(b"not json");
        assert_eq!((sid.as_str(), msg.as_str()), ("default", ""));
    }

    #[test]
    fn skill_subpaths_must_stay_relative() {
        assert!(is_plain_relative(""));
        assert!(is_plain_relative("skills/foo"));
        assert!(!is_plain_relative("../x"));
        assert!(!is_plain_relative("skills/../../x"));
        assert!(!is_plain_relative("/etc"));
    }

    #[cfg(unix)]
    #[test]
    fn copy_dir_skips_symlinks() {
        let base = std::env::temp_dir().join(format!("ira-copydir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (src, dest, outside) = (base.join("src"), base.join("dest"), base.join("outside"));
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret"), "s").unwrap();
        std::fs::write(src.join("SKILL.md"), "ok").unwrap();
        std::fs::write(src.join("sub").join("a.txt"), "a").unwrap();
        std::os::unix::fs::symlink(outside.join("secret"), src.join("linked.md")).unwrap();
        std::os::unix::fs::symlink(&outside, src.join("linkdir")).unwrap();
        std::os::unix::fs::symlink(&src, src.join("sub").join("loop")).unwrap();
        copy_dir(&src, &dest).unwrap();
        assert!(dest.join("SKILL.md").is_file());
        assert!(dest.join("sub").join("a.txt").is_file());
        assert!(std::fs::symlink_metadata(dest.join("linked.md")).is_err());
        assert!(std::fs::symlink_metadata(dest.join("linkdir")).is_err());
        assert!(std::fs::symlink_metadata(dest.join("sub").join("loop")).is_err());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn yaml_set_edits_fields_and_keeps_the_rest() {
        let raw = "name: a\ndescription: old\nmodel:\n  preferred: x\n  fallback: [y]\npermissions:\n  deny: [\"cli(rm)\"]\nsandbox:\n  wrapper: [w]\n";
        let mut doc: serde_yaml::Value = serde_yaml::from_str(raw).unwrap();
        yaml_set(&mut doc, &["description"], "new\ninjected: true".into());
        yaml_set(&mut doc, &["model", "preferred"], "z".into());
        yaml_set(&mut doc, &["runtime", "max_turns"], 7u64.into());
        let out = serde_yaml::to_string(&doc).unwrap();
        let back: crate::sdk::manifest::AgentManifest = serde_yaml::from_str(&out).unwrap();
        assert_eq!(back.description, "new\ninjected: true");
        assert_eq!(back.model.preferred, "z");
        assert_eq!(back.model.fallback, vec!["y".to_string()]);
        assert_eq!(back.max_turns(), 7);
        assert_eq!(back.permissions.unwrap().deny, vec!["cli(rm)".to_string()]);
        assert_eq!(back.sandbox.unwrap().wrapper, vec!["w".to_string()]);
        let top: serde_yaml::Value = serde_yaml::from_str(&out).unwrap();
        assert!(top.get("injected").is_none());
    }
}
