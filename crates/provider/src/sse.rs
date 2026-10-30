//! Server-sent events: splits a byte stream into the `data` payload of each event.

#[derive(Default)]
pub(crate) struct SseDecoder {
    buf: Vec<u8>,
}

impl SseDecoder {
    /// Adds bytes and returns the data of every event completed by them.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<String> {
        // JSON strings escape CR, so dropping it only normalizes line endings.
        self.buf.extend(bytes.iter().filter(|&&b| b != b'\r'));
        let mut events = Vec::new();
        while let Some(end) = self.buf.windows(2).position(|w| w == b"\n\n") {
            let event: Vec<u8> = self.buf.drain(..end + 2).collect();
            let text = String::from_utf8_lossy(&event);
            let data: Vec<&str> = text
                .lines()
                .filter_map(|l| l.strip_prefix("data:"))
                .map(|d| d.strip_prefix(' ').unwrap_or(d))
                .collect();
            if !data.is_empty() {
                events.push(data.join("\n"));
            }
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_events_across_chunks_and_line_endings() {
        let mut d = SseDecoder::default();
        assert!(d.feed(b"event: a\r\ndata: {\"x\":").is_empty());
        assert_eq!(
            d.feed(b"1}\r\n\r\n: comment\n\ndata: two\n\n"),
            ["{\"x\":1}", "two"]
        );
        assert_eq!(d.feed(b"data: a\ndata: b\n\n"), ["a\nb"]);
    }
}
