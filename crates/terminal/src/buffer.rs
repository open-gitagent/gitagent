//! Output buffer for one shell, with end-marker detection and a memory cap.

/// The parsed end-marker line `__GA_END_{seq}__ {exit} {cwd}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Marker {
    pub exit: i32,
    pub cwd: String,
}

pub(crate) struct Taken {
    pub output: Vec<u8>,
    pub omitted_bytes: u64,
}

pub(crate) fn marker_prefix(seq: u64) -> String {
    format!("__GA_END_{seq}__ ")
}

/// Raw bytes read from the PTY since the last result was taken.
///
/// When the buffer grows past `cap` bytes the middle is dropped, keeping the
/// first `cap / 4` and the last `3 * cap / 4` bytes and counting what was
/// dropped. The marker always arrives last, so dropping the middle never loses it.
pub(crate) struct OutputBuffer {
    data: Vec<u8>,
    cap: usize,
    /// Bytes before this index contain no marker start for the current command.
    scanned: usize,
    omitted: u64,
}

impl OutputBuffer {
    pub fn new(cap: usize) -> Self {
        OutputBuffer {
            data: Vec::new(),
            cap: cap.max(1024),
            scanned: 0,
            omitted: 0,
        }
    }

    fn head_len(&self) -> usize {
        self.cap / 4
    }

    pub fn push(&mut self, bytes: &[u8]) {
        self.data.extend_from_slice(bytes);
        if self.data.len() <= self.cap {
            return;
        }
        let head = self.head_len();
        let drop_end = self.data.len() - (self.cap - head);
        let dropped = drop_end - head;
        self.data.drain(head..drop_end);
        self.omitted += dropped as u64;
        self.scanned = if self.scanned >= drop_end {
            self.scanned - dropped
        } else {
            self.scanned.min(head)
        };
    }

    /// Looks for the complete marker line for `seq`. On success, returns the
    /// marker and the output before it, leaving anything after the marker line
    /// in the buffer.
    pub fn take_marker(&mut self, seq: u64) -> Option<(Marker, Taken)> {
        let needle = format!("\n{}", marker_prefix(seq)).into_bytes();
        let mut from = self.scanned;
        loop {
            let Some(rel) = find(&self.data[from..], &needle) else {
                // Leave room for a marker that is only partly received.
                self.scanned = self.data.len().saturating_sub(needle.len()).max(from);
                return None;
            };
            let start = from + rel;
            let line_start = start + needle.len();
            let Some(nl) = self.data[line_start..].iter().position(|&b| b == b'\n') else {
                // Marker line not complete yet.
                self.scanned = start;
                return None;
            };
            let line = String::from_utf8_lossy(&self.data[line_start..line_start + nl]);
            let (code, cwd) = line.split_once(' ').unwrap_or((&line, ""));
            let Ok(exit) = code.parse::<i32>() else {
                // Not our marker (for example the text of the printf format).
                from = line_start;
                continue;
            };
            let marker = Marker {
                exit,
                cwd: cwd.to_string(),
            };
            let rest = self.data.split_off(line_start + nl + 1);
            self.data.truncate(start);
            let taken = self.take_upto(self.data.len());
            self.data = rest;
            return Some((marker, taken));
        }
    }

    /// Takes the output received so far for a command that is still running,
    /// holding back any bytes that might be the start of its marker line.
    pub fn take_partial(&mut self, seq: u64) -> Taken {
        let needle = format!("\n{}", marker_prefix(seq)).into_bytes();
        let keep_from = match find(&self.data[self.scanned.min(self.data.len())..], &needle) {
            Some(rel) => self.scanned + rel,
            None => {
                let longest = (1..needle.len())
                    .rev()
                    .find(|&k| self.data.ends_with(&needle[..k]))
                    .unwrap_or(0);
                self.data.len() - longest
            }
        };
        self.take_upto(keep_from)
    }

    /// Takes everything buffered so far.
    pub fn take_all(&mut self) -> Taken {
        self.take_upto(self.data.len())
    }

    fn take_upto(&mut self, end: usize) -> Taken {
        let rest = self.data.split_off(end);
        let mut output = std::mem::replace(&mut self.data, rest);
        if self.omitted > 0 {
            let note = format!("\n[... {} bytes omitted ...]\n", self.omitted);
            let at = self.head_len().min(output.len());
            output.splice(at..at, note.into_bytes());
        }
        let taken = Taken {
            output,
            omitted_bytes: self.omitted,
        };
        self.scanned = 0;
        self.omitted = 0;
        taken
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_marker_split_across_pushes() {
        let mut b = OutputBuffer::new(1 << 20);
        b.push(b"hello\nworld\n__GA_EN");
        assert!(b.take_marker(3).is_none());
        b.push(b"D_3__ 42 /tmp/a b");
        assert!(b.take_marker(3).is_none());
        b.push(b"\nleftover");
        let (m, t) = b.take_marker(3).unwrap();
        assert_eq!(m.exit, 42);
        assert_eq!(m.cwd, "/tmp/a b");
        assert_eq!(t.output, b"hello\nworld");
        assert_eq!(b.take_all().output, b"leftover");
    }

    #[test]
    fn ignores_markers_for_other_commands_and_format_strings() {
        let mut b = OutputBuffer::new(1 << 20);
        b.push(b"x\n__GA_END_2__ 0 /\n\n__GA_END_3__ %s %s\ny\n__GA_END_3__ 1 /\n");
        let (m, t) = b.take_marker(3).unwrap();
        assert_eq!(m.exit, 1);
        assert_eq!(t.output, b"x\n__GA_END_2__ 0 /\n\n__GA_END_3__ %s %s\ny");
    }

    #[test]
    fn partial_take_holds_back_a_split_marker() {
        let mut b = OutputBuffer::new(1 << 20);
        b.push(b"progress 50%\n__GA_E");
        assert!(b.take_marker(9).is_none());
        assert_eq!(b.take_partial(9).output, b"progress 50%");
        b.push(b"ND_9__ 0 /\n");
        let (m, t) = b.take_marker(9).unwrap();
        assert_eq!(m.exit, 0);
        assert!(t.output.is_empty());
    }

    #[test]
    fn caps_memory_keeping_head_and_tail() {
        let mut b = OutputBuffer::new(4096);
        b.push(b"HEAD");
        for _ in 0..100 {
            b.push(&[b'x'; 1000]);
            assert!(b.take_marker(1).is_none());
        }
        b.push(b"TAIL\n__GA_END_1__ 0 /\n");
        let (_, t) = b.take_marker(1).unwrap();
        assert!(t.omitted_bytes > 90_000);
        let text = String::from_utf8(t.output).unwrap();
        assert!(text.starts_with("HEAD"));
        assert!(text.ends_with("TAIL"));
        assert!(text.contains("bytes omitted"));
        assert!(text.len() < 5000);
    }
}
