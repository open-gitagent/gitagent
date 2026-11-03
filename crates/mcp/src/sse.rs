//! Server-sent events, as MCP's HTTP transports stream them.

/// Splits a byte stream into events. Chunks may end anywhere, even inside
/// a character.
#[derive(Default)]
pub(crate) struct SseParser {
    buf: Vec<u8>,
}

/// One event: its name (`message` when none is given) and its data lines
/// joined with newlines.
#[derive(Debug, PartialEq)]
pub(crate) struct Event {
    pub name: String,
    pub data: String,
}

impl SseParser {
    pub fn push(&mut self, chunk: &[u8]) -> Vec<Event> {
        self.buf.extend(chunk.iter().filter(|b| **b != b'\r'));
        let mut events = Vec::new();
        while let Some(end) = self.buf.windows(2).position(|w| w == b"\n\n") {
            let block: Vec<u8> = self.buf.drain(..end + 2).collect();
            if let Some(e) = parse(&String::from_utf8_lossy(&block)) {
                events.push(e);
            }
        }
        events
    }
}

fn parse(block: &str) -> Option<Event> {
    let mut name = String::from("message");
    let mut data: Vec<&str> = Vec::new();
    for line in block.lines() {
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "event" => name = value.to_string(),
            "data" => data.push(value),
            _ => {}
        }
    }
    (!data.is_empty()).then(|| Event {
        name,
        data: data.join("\n"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_survive_any_chunking() {
        let stream = "event: endpoint\r\ndata: /messages?s=1\r\n\r\n: keep-alive\n\ndata: {\"a\":\ndata: 1}\n\nevent: message\ndata: é\n\n";
        let expected = vec![
            Event {
                name: "endpoint".into(),
                data: "/messages?s=1".into(),
            },
            Event {
                name: "message".into(),
                data: "{\"a\":\n1}".into(),
            },
            Event {
                name: "message".into(),
                data: "é".into(),
            },
        ];
        for size in [1, 2, 3, 7, stream.len()] {
            let mut p = SseParser::default();
            let mut got = Vec::new();
            for chunk in stream.as_bytes().chunks(size) {
                got.extend(p.push(chunk));
            }
            assert_eq!(got, expected, "chunk size {size}");
        }
    }
}
