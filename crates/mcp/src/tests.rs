use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

use super::*;

/// A stdio server that pages its tool list, sends a ping of its own before
/// answering, and fails one tool with a protocol error.
const FAKE_SERVER: &str = r#"while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"fake","version":"1"}}}\n' "$id" ;;
    *'"method":"tools/list"'*'"cursor":"p2"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"env","inputSchema":{"type":"object"}}]}}\n' "$id" ;;
    *'"method":"tools/list"'*)
      printf '{"jsonrpc":"2.0","id":"srv-1","method":"ping"}\n'
      printf '{"jsonrpc":"2.0","method":"notifications/message","params":{}}\n'
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"echo","description":"Echoes.","inputSchema":{"type":"object","properties":{"text":{"type":"string"}}}},{"name":"bad.name/x"}],"nextCursor":"p2"}}\n' "$id" ;;
    *'"name":"echo"'*)
      text=$(printf '%s' "$line" | sed -n 's/.*"text":"\([^"]*\)".*/\1/p')
      printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"echo: %s"},{"type":"image","data":"iVBO","mimeType":"image/png"}]}}\n' "$id" "$text" ;;
    *'"name":"env"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"%s %s"}],"isError":true}}\n' "$id" "$FROM_CFG" "${GITAGENT_TEST_MCP_SECRET:-unset}" ;;
    *'"name":"bad.name/x"'*)
      printf '{"jsonrpc":"2.0","id":%s,"error":{"code":-32602,"message":"bad arguments"}}\n' "$id" ;;
    *'"id":"srv-1"'*)
      printf '%s\n' "$line" > "$PONG_FILE" ;;
  esac
done
"#;

fn stdio(name: &str, script: &std::path::Path, env: &[(&str, &str)], timeout: u64) -> McpServerCfg {
    McpServerCfg {
        name: name.into(),
        transport: McpTransport::Stdio {
            command: "sh".into(),
            args: vec![script.display().to_string()],
            env: env
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            cwd: None,
        },
        timeout: Duration::from_secs(timeout),
    }
}

fn args(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

#[tokio::test]
async fn stdio_servers_list_page_and_call_tools() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("server.sh");
    std::fs::write(&script, FAKE_SERVER).unwrap();
    let pong = dir.path().join("pong");
    std::env::set_var("GITAGENT_TEST_MCP_SECRET", "leaked");
    let cfg = stdio(
        "fake",
        &script,
        &[
            ("FROM_CFG", "${GITAGENT_TEST_MCP_FROM}"),
            ("PONG_FILE", &pong.display().to_string()),
        ],
        10,
    );
    std::env::set_var("GITAGENT_TEST_MCP_FROM", "from-config");
    let (servers, warnings) = Servers::connect(&[cfg], &[]).await;
    assert!(warnings.is_empty(), "{warnings:?}");
    let names: Vec<String> = servers.specs().into_iter().map(|t| t.name).collect();
    assert_eq!(names, vec!["fake__echo", "fake__bad_name_x", "fake__env"]);
    let specs = servers.specs();
    assert_eq!(specs[0].description, "Echoes.");
    assert_eq!(specs[1].description, "MCP tool bad.name/x from fake");
    assert_eq!(
        specs[2].input_schema,
        json!({"type": "object", "properties": {}})
    );

    let out = servers
        .call(
            "fake__echo",
            &args(json!({"text": "hi"})),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
    assert_eq!(out.text, "echo: hi");
    assert_eq!(out.images[0].media_type, "image/png");
    assert!(!out.is_error);

    // The server sees its own variables and the configured ones, not ours.
    let out = servers
        .call("fake__env", &Map::new(), Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(
        (out.text.as_str(), out.is_error),
        ("from-config unset", true)
    );

    let err = servers
        .call("fake__bad_name_x", &Map::new(), Duration::from_secs(5))
        .await
        .unwrap_err();
    assert_eq!(err, "bad arguments (-32602)");

    let mut answered = String::new();
    for _ in 0..50 {
        answered = std::fs::read_to_string(&pong).unwrap_or_default();
        if !answered.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        answered.trim(),
        r#"{"id":"srv-1","jsonrpc":"2.0","result":{}}"#
    );
}

#[tokio::test]
async fn bad_servers_and_taken_names_are_left_out() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("server.sh");
    std::fs::write(&script, FAKE_SERVER).unwrap();
    let silent = dir.path().join("silent.sh");
    std::fs::write(&silent, "echo starting >&2; sleep 10\n").unwrap();
    let missing = McpServerCfg {
        name: "missing".into(),
        transport: McpTransport::Stdio {
            command: "/nonexistent/server".into(),
            args: vec![],
            env: BTreeMap::new(),
            cwd: None,
        },
        timeout: Duration::from_secs(5),
    };
    let (servers, warnings) = Servers::connect(
        &[
            stdio("fake", &script, &[], 10),
            stdio("silent", &silent, &[], 1),
            missing,
        ],
        &["fake__echo".to_string()],
    )
    .await;
    assert_eq!(servers.specs().len(), 2);
    assert!(!servers.has("fake__echo"));
    assert_eq!(warnings.len(), 3, "{warnings:?}");
    assert!(warnings[0].contains("\"fake__echo\" is already taken"));
    assert!(warnings[1].starts_with("MCP server \"silent\" (stdio): no answer within 1s"));
    assert!(warnings[2].contains("cannot start /nonexistent/server"));
}

#[test]
fn names_and_results() {
    assert_eq!(tool_name("git hub", "search/code"), "git_hub__search_code");
    assert_eq!(tool_name(&"s".repeat(40), &"t".repeat(40)).len(), 64);
    let out = flatten(&json!({"content": [
        {"type": "resource", "resource": {"uri": "file:///a", "text": "body"}},
        {"type": "resource", "resource": {"uri": "file:///b.bin"}},
        {"type": "resource_link", "uri": "file:///c"},
        {"type": "audio", "mimeType": "audio/wav", "data": "x"}
    ]}));
    assert_eq!(
        out.text,
        "body\n[resource: file:///b.bin (binary)]\n[resource_link: file:///c]\n[audio: audio/wav, data omitted]"
    );
    assert_eq!(
        flatten(&json!({"content": [], "structuredContent": {"n": 1}})).text,
        r#"{"n":1}"#
    );
}

/// What a test HTTP server answers.
enum Body {
    Full(String),
    /// An event stream fed by the test until the sender is dropped.
    Stream(mpsc::UnboundedReceiver<String>),
}

struct Reply {
    status: u16,
    content_type: &'static str,
    headers: Vec<(&'static str, String)>,
    body: Body,
}

/// One request as the test server saw it: method, path, lower-case headers
/// and body.
type Seen = (String, String, BTreeMap<String, String>, String);

/// Serves each connection with `handler` and returns the base URL.
async fn serve<F>(handler: F) -> String
where
    F: Fn(&Seen) -> Reply + Send + Sync + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handler = Arc::new(handler);
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let handler = handler.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let head_end = loop {
                    let n = sock.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        break i;
                    }
                };
                let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
                let mut lines = head.lines();
                let first: Vec<&str> = lines.next().unwrap_or("").split(' ').collect();
                let headers: BTreeMap<String, String> = lines
                    .filter_map(|l| l.split_once(':'))
                    .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
                    .collect();
                let len: usize = headers
                    .get("content-length")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                let mut body = buf[head_end + 4..].to_vec();
                while body.len() < len {
                    let n = sock.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    body.extend_from_slice(&chunk[..n]);
                }
                let seen = (
                    first[0].to_string(),
                    first.get(1).unwrap_or(&"").to_string(),
                    headers,
                    String::from_utf8_lossy(&body).to_string(),
                );
                let reply = handler(&seen);
                let mut head = format!(
                    "HTTP/1.1 {} X\r\ncontent-type: {}\r\nconnection: close\r\n",
                    reply.status, reply.content_type
                );
                for (k, v) in &reply.headers {
                    head.push_str(&format!("{k}: {v}\r\n"));
                }
                match reply.body {
                    Body::Full(text) => {
                        head.push_str(&format!("content-length: {}\r\n\r\n{text}", text.len()));
                        let _ = sock.write_all(head.as_bytes()).await;
                    }
                    Body::Stream(mut rx) => {
                        head.push_str("\r\n");
                        let _ = sock.write_all(head.as_bytes()).await;
                        while let Some(event) = rx.recv().await {
                            if sock.write_all(event.as_bytes()).await.is_err() {
                                break;
                            }
                        }
                    }
                }
                let _ = sock.shutdown().await;
            });
        }
    });
    format!("http://{addr}")
}

fn rpc_id(body: &str) -> Value {
    serde_json::from_str::<Value>(body).map_or(Value::Null, |v| v["id"].clone())
}

fn answer(body: &str) -> String {
    let id = rpc_id(body);
    let result = if body.contains("\"initialize\"") {
        json!({"protocolVersion": "2025-03-26", "capabilities": {}, "serverInfo": {"name": "h"}})
    } else if body.contains("tools/list") {
        json!({"tools": [{"name": "add", "inputSchema": {"type": "object"}}]})
    } else {
        json!({"content": [{"type": "text", "text": "3"}]})
    };
    json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string()
}

#[tokio::test]
async fn streamable_http_keeps_the_session_and_reads_json_or_events() {
    let seen: Arc<Mutex<Vec<Seen>>> = Arc::default();
    let log = seen.clone();
    let url = serve(move |req| {
        log.lock().unwrap().push(req.clone());
        let body = &req.3;
        if body.contains("notifications/initialized") {
            return Reply {
                status: 202,
                content_type: "text/plain",
                headers: vec![],
                body: Body::Full(String::new()),
            };
        }
        if body.contains("tools/list") {
            let (tx, rx) = mpsc::unbounded_channel();
            tx.send("event: message\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\n\n".into())
                .unwrap();
            tx.send(format!("data: {}\n\n", answer(body))).unwrap();
            return Reply {
                status: 200,
                content_type: "text/event-stream",
                headers: vec![],
                body: Body::Stream(rx),
            };
        }
        Reply {
            status: 200,
            content_type: "application/json",
            headers: vec![("mcp-session-id", "sess-1".into())],
            body: Body::Full(answer(body)),
        }
    })
    .await;
    std::env::set_var("GITAGENT_TEST_MCP_TOKEN", "t0k");
    let cfg = McpServerCfg {
        name: "calc".into(),
        transport: McpTransport::Http {
            url: format!("{url}/mcp"),
            headers: [(
                "Authorization".to_string(),
                "Bearer ${GITAGENT_TEST_MCP_TOKEN}".to_string(),
            )]
            .into(),
        },
        timeout: Duration::from_secs(5),
    };
    let (servers, warnings) = Servers::connect(&[cfg], &[]).await;
    assert!(warnings.is_empty(), "{warnings:?}");
    let out = servers
        .call(
            "calc__add",
            &args(json!({"a": 1, "b": 2})),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
    assert_eq!(out.text, "3");

    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 4);
    assert!(seen.iter().all(|r| r.0 == "POST" && r.1 == "/mcp"));
    assert!(seen.iter().all(|r| r.2["authorization"] == "Bearer t0k"));
    assert!(!seen[0].2.contains_key("mcp-session-id"));
    for later in &seen[1..] {
        assert_eq!(later.2["mcp-session-id"], "sess-1");
        assert_eq!(later.2["mcp-protocol-version"], "2025-03-26");
    }
}

#[tokio::test]
async fn the_older_sse_transport_answers_on_its_stream() {
    let stream: Arc<Mutex<Option<mpsc::UnboundedSender<String>>>> = Arc::default();
    let s = stream.clone();
    let url = serve(move |req| {
        if req.0 == "GET" {
            let (tx, rx) = mpsc::unbounded_channel();
            tx.send("event: endpoint\ndata: /messages?session=7\n\n".into())
                .unwrap();
            *s.lock().unwrap() = Some(tx);
            return Reply {
                status: 200,
                content_type: "text/event-stream",
                headers: vec![],
                body: Body::Stream(rx),
            };
        }
        assert_eq!(req.1, "/messages?session=7");
        if !rpc_id(&req.3).is_null() {
            let tx = s.lock().unwrap().clone().unwrap();
            tx.send(format!("event: message\ndata: {}\n\n", answer(&req.3)))
                .unwrap();
        }
        Reply {
            status: 202,
            content_type: "text/plain",
            headers: vec![],
            body: Body::Full("Accepted".into()),
        }
    })
    .await;
    let cfg = McpServerCfg {
        name: "old".into(),
        transport: McpTransport::Sse {
            url: format!("{url}/sse"),
            headers: BTreeMap::new(),
        },
        timeout: Duration::from_secs(5),
    };
    let (servers, warnings) = Servers::connect(&[cfg], &[]).await;
    assert!(warnings.is_empty(), "{warnings:?}");
    let out = servers
        .call("old__add", &Map::new(), Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(out.text, "3");
}
