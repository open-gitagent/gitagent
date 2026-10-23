//! The audit log (`compliance.recordkeeping`): one JSON object per line in
//! `<audit dir>/audit-YYYYMMDD.jsonl`, only ever appended to. What is
//! recorded follows `log_contents`: `prompts_and_responses`, `tool_calls`,
//! `decision_pathways` (the model's reasoning), `model_version`; approvals,
//! blocks and the end of each run are always recorded, with timestamps.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use gitagent_loader::AuditCfg;
use serde_json::{json, Map, Value};

/// Most characters of a tool result kept in a record.
const RESULT_CHARS: usize = 1000;

pub struct AuditLog {
    cfg: AuditCfg,
    session_id: String,
    lock: Mutex<()>,
}

impl AuditLog {
    pub fn new(cfg: &AuditCfg, session_id: &str) -> Self {
        AuditLog {
            cfg: cfg.clone(),
            session_id: session_id.to_string(),
            lock: Mutex::new(()),
        }
    }

    /// Today's file.
    fn path(&self) -> PathBuf {
        if let Some(f) = &self.cfg.file {
            return f.clone();
        }
        let now = gitagent_record::utc_now_rfc3339();
        let day: String = now.chars().take(10).filter(|c| *c != '-').collect();
        self.cfg.dir.join(format!("audit-{day}.jsonl"))
    }

    fn write(&self, event: &str, fields: Map<String, Value>) {
        let mut record = Map::new();
        record.insert(
            "timestamp".into(),
            json!(gitagent_record::utc_now_rfc3339()),
        );
        record.insert("session_id".into(), json!(self.session_id));
        record.insert("event".into(), json!(event));
        record.extend(fields);
        let line = format!("{}\n", Value::Object(record));
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        // Append only; the file is never rewritten.
        let path = self.path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let opened = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&path);
        if let Ok(mut f) = opened {
            let _ = f.write_all(line.as_bytes());
        }
    }

    /// The instruction a run starts with.
    pub fn start(&self, instruction: &str) {
        if self.cfg.logs("prompts_and_responses") {
            self.write("prompt", object(json!({"text": instruction})));
        } else {
            self.write("run_start", Map::new());
        }
    }

    /// One of the run's events, as a record when it is one to keep.
    pub fn record(&self, e: &Value) {
        let cfg = &self.cfg;
        let turn = e.get("turn").cloned().unwrap_or(Value::Null);
        match e["event"].as_str().unwrap_or_default() {
            "model_text" => {
                let mut fields = Map::new();
                fields.insert("turn".into(), turn.clone());
                if cfg.logs("model_version") {
                    fields.insert("model".into(), e["model"].clone());
                }
                if cfg.logs("prompts_and_responses") {
                    fields.insert("text".into(), e["text"].clone());
                    fields.insert("tool_calls".into(), e["tool_calls"].clone());
                    self.write("response", fields.clone());
                } else if cfg.logs("model_version") {
                    self.write("response", fields.clone());
                }
                if cfg.logs("decision_pathways") {
                    if let Some(r) = e["reasoning"].as_str().filter(|r| !r.is_empty()) {
                        self.write("reasoning", object(json!({"turn": turn, "reasoning": r})));
                    }
                }
            }
            "steer" if cfg.logs("prompts_and_responses") => {
                self.write("prompt", object(json!({"turn": turn, "text": e["text"]})));
            }
            "tool_call" if cfg.logs("tool_calls") => {
                self.write(
                    "tool_use",
                    object(json!({"turn": turn, "id": e["id"], "tool": e["name"], "args": e["arguments"]})),
                );
            }
            "tool_result" if cfg.logs("tool_calls") => {
                let content = e
                    .get("output")
                    .or(e.get("content"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let cut: String = content.chars().take(RESULT_CHARS).collect();
                self.write(
                    "tool_result",
                    object(json!({"turn": turn, "id": e["id"], "tool": e["name"], "is_error": e["is_error"], "result": cut})),
                );
            }
            "approval_request" => self.write(
                "approval_request",
                object(json!({"turn": turn, "id": e["id"], "tool": e["name"], "reason": e["reason"]})),
            ),
            "approval" => self.write(
                "approval",
                object(json!({"turn": turn, "id": e["id"], "approved": e["approved"], "reason": e["reason"]})),
            ),
            "guard" if e["kind"] == "hook_block" => {
                self.write("blocked", object(json!({"turn": turn, "reason": e["reason"]})));
            }
            "run_end" => self.write(
                "run_end",
                object(json!({"outcome": e["outcome"], "turns": e["turns"], "usage": e["usage"]})),
            ),
            _ => {}
        }
    }
}

fn object(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}
