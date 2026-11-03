//! Runs the tools an agent declares in `tools/*.yaml`. A script gets the
//! call's arguments as one JSON object on stdin; an HTTP tool sends them as
//! the body or the query. What comes back is read the way 1.x read it: a
//! `data:image/…;base64,` line is an image, a JSON object's `text` or
//! `result` is the answer (`error` makes it an error), MCP-style `content`
//! blocks are joined, and anything else is the text itself.

use std::process::Stdio;
use std::time::Duration;

use anyhow::Result;
use gitagent_loader::declared::{expand_env, Runner};
use gitagent_loader::DeclaredTool;
use gitagent_provider::{ImageData, ToolCall, ToolSpec};
use serde_json::{Map, Value};
use tokio::io::AsyncWriteExt;

use crate::ToolOutput;

/// Most characters of a result kept (half from the start, half from the end).
const MAX_CHARS: usize = 50_000;

pub fn spec(tool: &DeclaredTool) -> ToolSpec {
    ToolSpec {
        name: tool.name.clone(),
        description: tool.description.clone(),
        input_schema: tool.input_schema.clone(),
    }
}

/// Runs a declared tool: the arguments are checked against its schema,
/// then the script or request is made.
pub async fn run(
    call: &ToolCall,
    tool: &DeclaredTool,
    remaining: Option<Duration>,
    env: &std::collections::BTreeMap<String, String>,
) -> Result<ToolOutput> {
    if let Some(problem) = check_arguments(&call.arguments, &tool.input_schema) {
        return Ok(ToolOutput::error(format!(
            "Invalid arguments for {}: {problem}.{}",
            tool.name,
            expected(&tool.input_schema)
        )));
    }
    let timeout = remaining.map_or(tool.timeout, |r| r.min(tool.timeout));
    let out = match &tool.runner {
        Runner::Script { command, dir } => {
            run_script(command, dir, &call.arguments, timeout, env, &tool.env).await
        }
        Runner::Http {
            url,
            method,
            headers,
        } => run_http(url, method, headers, &call.arguments, timeout).await,
        Runner::Mcp { url } => {
            return Ok(
                match gitagent_mcp::call_once(url, &tool.name, &call.arguments, timeout).await {
                    Ok(out) => crate::mcp_output(out),
                    Err(why) => {
                        ToolOutput::error(format!("Tool {} failed: {}", tool.name, cap(&why)))
                    }
                },
            )
        }
    };
    Ok(match out {
        Ok(text) => read_result(&text),
        Err(why) => ToolOutput::error(format!("Tool {} failed: {}", tool.name, cap(&why))),
    })
}

async fn run_script(
    command: &[String],
    dir: &std::path::Path,
    args: &Map<String, Value>,
    timeout: Duration,
    env: &std::collections::BTreeMap<String, String>,
    own_env: &std::collections::BTreeMap<String, String>,
) -> Result<String, String> {
    let (program, rest) = command.split_first().ok_or("no command to run")?;
    let mut child = tokio::process::Command::new(program)
        .args(rest)
        .envs(env.iter().map(|(k, v)| (k, expand_env(v))))
        .envs(own_env)
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("cannot start {program}: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let input = Value::Object(args.clone()).to_string();
        // A script that never reads its input closes the pipe early; that is
        // not an error.
        let _ = stdin.write_all(input.as_bytes()).await;
    }
    let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(r) => r.map_err(|e| e.to_string())?,
        Err(_) => return Err(format!("timed out after {}", secs(timeout))),
    };
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    if output.status.success() {
        return Ok(stdout);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail = if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    };
    Err(match output.status.code() {
        Some(code) => format!("exited with code {code}: {detail}"),
        None => format!("was killed: {detail}"),
    })
}

async fn run_http(
    url: &str,
    method: &str,
    headers: &std::collections::BTreeMap<String, String>,
    args: &Map<String, Value>,
    timeout: Duration,
) -> Result<String, String> {
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|e| e.to_string())?;
    let method = reqwest::Method::from_bytes(method.as_bytes()).map_err(|e| e.to_string())?;
    let mut req = client.request(method.clone(), expand_env(url));
    for (k, v) in headers {
        req = req.header(k, expand_env(v));
    }
    req = if method == reqwest::Method::GET || method == reqwest::Method::DELETE {
        let query: Vec<(&str, String)> = args
            .iter()
            .map(|(k, v)| {
                let v = match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                (k.as_str(), v)
            })
            .collect();
        req.query(&query)
    } else {
        req.header("content-type", "application/json")
            .body(Value::Object(args.clone()).to_string())
    };
    let resp = req.send().await.map_err(|e| {
        if e.is_timeout() {
            format!("timed out after {}", secs(timeout))
        } else {
            e.to_string()
        }
    })?;
    let status = resp.status();
    let body = resp.text().await.map_err(|e| e.to_string())?;
    if status.is_success() {
        Ok(body)
    } else {
        Err(format!("HTTP {status}: {}", body.trim()))
    }
}

/// A tool's output as a result for the model.
fn read_result(text: &str) -> ToolOutput {
    let text = text.trim();
    if let Some(image) = text.lines().find_map(data_uri_image) {
        return ToolOutput {
            images: vec![image],
            ..ToolOutput::ok("Image returned by the tool.")
        };
    }
    if let Ok(Value::Object(v)) = serde_json::from_str::<Value>(text) {
        // `error: false`, `error: null` and `error: ""` mean no error.
        let is_error = match v.get("error") {
            Some(Value::String(s)) => !s.trim().is_empty(),
            Some(Value::Bool(b)) => *b,
            Some(Value::Object(o)) => !o.is_empty(),
            Some(Value::Array(a)) => !a.is_empty(),
            Some(Value::Number(n)) => n.as_f64() != Some(0.0),
            Some(Value::Null) | None => false,
        };
        if is_error {
            let e = &v["error"];
            let e = e.as_str().map_or_else(|| e.to_string(), str::to_string);
            return ToolOutput::error(cap(&e));
        }
        if let Some(t) = v.get("text").and_then(Value::as_str) {
            return ToolOutput::ok(cap(t));
        }
        if let Some(r) = v.get("result") {
            let r = r.as_str().map_or_else(|| r.to_string(), str::to_string);
            return ToolOutput::ok(cap(&r));
        }
        if let Some(Value::Array(blocks)) = v.get("content") {
            let texts: Vec<&str> = blocks
                .iter()
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect();
            if !texts.is_empty() {
                let is_error = v.get("isError").and_then(Value::as_bool) == Some(true);
                let joined = cap(&texts.join("\n"));
                return if is_error {
                    ToolOutput::error(joined)
                } else {
                    ToolOutput::ok(joined)
                };
            }
        }
    }
    if text.is_empty() {
        ToolOutput::ok("(no output)")
    } else {
        ToolOutput::ok(cap(text))
    }
}

fn data_uri_image(line: &str) -> Option<ImageData> {
    let rest = line.trim().strip_prefix("data:")?;
    let (media_type, data) = rest.split_once(";base64,")?;
    let data: String = data.chars().filter(|c| !c.is_whitespace()).collect();
    let known = matches!(
        media_type,
        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
    );
    (known && !data.is_empty()).then(|| ImageData {
        media_type: media_type.to_string(),
        data,
    })
}

fn secs(d: Duration) -> String {
    if d.subsec_millis() == 0 {
        format!("{}s", d.as_secs())
    } else {
        format!("{:.1}s", d.as_secs_f64())
    }
}

pub(crate) fn cap(text: &str) -> String {
    let n = text.chars().count();
    if n <= MAX_CHARS {
        return text.to_string();
    }
    let half = MAX_CHARS / 2;
    let head: String = text.chars().take(half).collect();
    let tail: String = text.chars().skip(n - half).collect();
    format!("{head}\n[… {} characters cut …]\n{tail}", n - MAX_CHARS)
}

/// What is wrong with the arguments, checked against the schema's required
/// properties and the types of the top-level ones.
fn check_arguments(args: &Map<String, Value>, schema: &Value) -> Option<String> {
    let props = schema.get("properties").and_then(Value::as_object);
    for name in schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        if args.get(name).is_none_or(Value::is_null) {
            return Some(format!("missing required argument `{name}`"));
        }
    }
    for (name, value) in args {
        let Some(want) = props.and_then(|p| p.get(name)).and_then(|p| p.get("type")) else {
            continue;
        };
        let types: Vec<&str> = match want {
            Value::String(t) => vec![t.as_str()],
            Value::Array(ts) => ts.iter().filter_map(Value::as_str).collect(),
            _ => continue,
        };
        if !types.iter().any(|t| has_type(value, t)) {
            return Some(format!("`{name}` must be {}", types.join(" or ")));
        }
    }
    None
}

fn has_type(v: &Value, t: &str) -> bool {
    match t {
        "string" => v.is_string(),
        "number" => v.is_number(),
        "integer" => v.is_i64() || v.is_u64() || v.as_f64().is_some_and(|f| f.fract() == 0.0),
        "boolean" => v.is_boolean(),
        "array" => v.is_array(),
        "object" => v.is_object(),
        "null" => v.is_null(),
        _ => true,
    }
}

fn expected(schema: &Value) -> String {
    let required: Vec<&str> = schema["required"]
        .as_array()
        .map(|r| r.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let parts: Vec<String> = schema["properties"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(name, p)| {
            let ty = p["type"].as_str().unwrap_or("value");
            if required.contains(&name.as_str()) {
                format!("{name} ({ty}, required)")
            } else {
                format!("{name} ({ty})")
            }
        })
        .collect();
    if parts.is_empty() {
        String::new()
    } else {
        format!(" Expected arguments: {}.", parts.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Duration;

    fn script_tool(dir: &std::path::Path, body: &str, timeout: Duration) -> DeclaredTool {
        let path = dir.join("t.sh");
        std::fs::write(&path, body).unwrap();
        DeclaredTool {
            name: "t".into(),
            description: "d".into(),
            input_schema: json!({"type": "object", "properties": {"n": {"type": "integer"}, "s": {"type": "string"}}, "required": ["n"]}),
            runner: Runner::Script {
                command: vec!["sh".into(), path.display().to_string()],
                dir: dir.to_path_buf(),
            },
            timeout,
            requires_confirmation: false,
            read_only: false,
            env: Default::default(),
        }
    }

    fn call(args: Value) -> ToolCall {
        ToolCall {
            id: "1".into(),
            name: "t".into(),
            arguments: args.as_object().unwrap().clone(),
        }
    }

    #[tokio::test]
    async fn scripts_get_json_on_stdin_and_answer_on_stdout() {
        let dir = tempfile::tempdir().unwrap();
        let t = script_tool(
            dir.path(),
            "read input; printf '{\"result\": %s}' \"$input\"\n",
            Duration::from_secs(10),
        );
        let out = run(
            &call(json!({"n": 3, "s": "x"})),
            &t,
            None,
            &Default::default(),
        )
        .await
        .unwrap();
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(out.content, r#"{"n":3,"s":"x"}"#);

        let bad = run(&call(json!({"s": 1})), &t, None, &Default::default())
            .await
            .unwrap();
        assert!(bad.is_error);
        assert!(bad.content.contains("missing required argument `n`"));
        assert!(bad.content.contains("n (integer, required), s (string)"));
        let bad = run(&call(json!({"n": "3"})), &t, None, &Default::default())
            .await
            .unwrap();
        assert!(bad.content.contains("`n` must be integer"));
    }

    #[tokio::test]
    async fn scripts_get_the_environments_variables() {
        let dir = tempfile::tempdir().unwrap();
        let t = script_tool(
            dir.path(),
            "cat >/dev/null; printf '%s/%s' \"$API_BASE\" \"$REGION\"\n",
            Duration::from_secs(10),
        );
        std::env::set_var("GITAGENT_TEST_REGION", "eu");
        let env = [
            ("API_BASE".to_string(), "https://staging".to_string()),
            ("REGION".to_string(), "${GITAGENT_TEST_REGION}".to_string()),
        ]
        .into();
        let out = run(&call(json!({"n": 1})), &t, None, &env).await.unwrap();
        assert_eq!(out.content, "https://staging/eu");
        // A plugin's settings come last and are not expanded again.
        let mut t = t;
        t.env = [("REGION".to_string(), "${not-a-var}".to_string())].into();
        let out = run(&call(json!({"n": 1})), &t, None, &env).await.unwrap();
        assert_eq!(out.content, "https://staging/${not-a-var}");
    }

    #[tokio::test]
    async fn failures_and_timeouts_are_errors() {
        let dir = tempfile::tempdir().unwrap();
        let t = script_tool(
            dir.path(),
            "echo 'bad input' >&2; exit 3\n",
            Duration::from_secs(10),
        );
        let out = run(&call(json!({"n": 1})), &t, None, &Default::default())
            .await
            .unwrap();
        assert!(out.is_error);
        assert_eq!(out.content, "Tool t failed: exited with code 3: bad input");

        let slow = script_tool(dir.path(), "sleep 5\n", Duration::from_secs(10));
        let out = run(
            &call(json!({"n": 1})),
            &slow,
            Some(Duration::from_millis(200)),
            &Default::default(),
        )
        .await
        .unwrap();
        assert!(out.is_error && out.content.ends_with("timed out after 0.2s"));
    }

    #[test]
    fn results_are_read_like_1x() {
        assert_eq!(read_result("  plain\n").content, "plain");
        assert_eq!(read_result("").content, "(no output)");
        assert_eq!(read_result(r#"{"text": "hi"}"#).content, "hi");
        assert_eq!(read_result(r#"{"result": {"a": 1}}"#).content, r#"{"a":1}"#);
        let fine = read_result(r#"{"ok": true, "error": false, "result": "42"}"#);
        assert!(!fine.is_error && fine.content == "42");
        assert!(!read_result(r#"{"error": null, "text": "t"}"#).is_error);
        assert!(!read_result(r#"{"error": "", "text": "t"}"#).is_error);
        assert!(read_result(r#"{"error": {"code": 7}}"#).is_error);
        let e = read_result(r#"{"error": "no such user"}"#);
        assert!(e.is_error && e.content == "no such user");
        let m = read_result(
            r#"{"content": [{"type": "text", "text": "a"}, {"type": "text", "text": "b"}]}"#,
        );
        assert_eq!(m.content, "a\nb");
        let img = read_result("log line\ndata:image/png;base64,iVBO Rw==\n");
        assert_eq!(img.images[0].media_type, "image/png");
        assert_eq!(img.images[0].data, "iVBORw==");
        let big = "x".repeat(MAX_CHARS + 10);
        assert!(read_result(&big)
            .content
            .contains("[… 10 characters cut …]"));
    }
}
