//! `--events jsonl`: one JSON object per line, for logs and tools that drive
//! gitagent. Nothing secret is ever put in an event.

use std::io::Write;
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value};

/// Writes events as JSON lines, or hands each one to a callback. Disabled
/// sinks drop everything.
#[derive(Clone, Default)]
pub struct Events {
    out: Option<Arc<Mutex<Box<dyn Write + Send>>>>,
    callbacks: Vec<Arc<dyn Fn(Value) + Send + Sync>>,
}

impl Events {
    pub fn disabled() -> Self {
        Events::default()
    }

    /// Every event goes to `f` as the same object the JSON lines carry.
    pub fn to_callback(f: impl Fn(Value) + Send + Sync + 'static) -> Self {
        Events {
            out: None,
            callbacks: vec![Arc::new(f)],
        }
    }

    /// The same sink with `f` told about every event as well.
    pub fn also(mut self, f: impl Fn(Value) + Send + Sync + 'static) -> Self {
        self.callbacks.push(Arc::new(f));
        self
    }

    pub fn stdout() -> Self {
        Self::to_writer(Box::new(std::io::stdout()))
    }

    pub fn to_writer(w: Box<dyn Write + Send>) -> Self {
        Events {
            out: Some(Arc::new(Mutex::new(w))),
            callbacks: Vec::new(),
        }
    }

    pub fn enabled(&self) -> bool {
        self.out.is_some() || !self.callbacks.is_empty()
    }

    /// Emits `{"event": kind, "ts": ..., ...fields}`. Write errors are ignored:
    /// a closed pipe must not stop the run.
    pub fn emit(&self, kind: &str, fields: Value) {
        if self.out.is_none() && self.callbacks.is_empty() {
            return;
        }
        let mut obj = Map::new();
        obj.insert("event".into(), Value::String(kind.into()));
        obj.insert("ts".into(), Value::String(crate::utc_now_rfc3339()));
        if let Value::Object(f) = fields {
            obj.extend(f);
        }
        for f in &self.callbacks {
            f(Value::Object(obj.clone()));
        }
        let Some(out) = &self.out else {
            return;
        };
        let mut line = serde_json::to_string(&Value::Object(obj)).unwrap_or_default();
        line.push('\n');
        if let Ok(mut w) = out.lock() {
            let _ = w.write_all(line.as_bytes());
            let _ = w.flush();
        }
    }
}

/// A shared in-memory buffer, for tests that read events back.
#[derive(Clone, Default)]
pub struct SharedBuffer(pub Arc<Mutex<Vec<u8>>>);

impl Write for SharedBuffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl SharedBuffer {
    pub fn lines(&self) -> Vec<Value> {
        String::from_utf8_lossy(&self.0.lock().unwrap())
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn writes_one_object_per_line() {
        let buf = SharedBuffer::default();
        let events = Events::to_writer(Box::new(buf.clone()));
        events.emit("turn_start", json!({"turn": 1}));
        events.emit("run_end", json!({"outcome": "completed"}));
        let lines = buf.lines();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["event"], "turn_start");
        assert_eq!(lines[0]["turn"], 1);
        assert!(lines[0]["ts"].as_str().unwrap().ends_with('Z'));
        assert_eq!(lines[1]["outcome"], "completed");
        // A disabled sink does nothing.
        Events::disabled().emit("x", json!({}));
    }
}
