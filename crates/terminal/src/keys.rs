//! Turns the text a model sends to `write_stdin` into bytes for the terminal.
//!
//! Models usually write control keys as escape sequences in plain text
//! (`\x03` for Ctrl-C), so those are decoded here. Real control characters
//! that arrive already decoded pass through unchanged.

/// Decodes `\xHH`, `\e`, `\n`, `\r`, `\t`, `\0` and `\\`. Anything else after a
/// backslash is kept as written.
pub fn decode_keys(s: &str) -> Vec<u8> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' || i + 1 >= bytes.len() {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        let next = bytes[i + 1];
        let simple = match next {
            b'e' => Some(0x1b),
            b'n' => Some(b'\n'),
            b'r' => Some(b'\r'),
            b't' => Some(b'\t'),
            b'0' => Some(0),
            b'\\' => Some(b'\\'),
            _ => None,
        };
        if let Some(b) = simple {
            out.push(b);
            i += 2;
        } else if next == b'x' && hex_pair(&bytes[i + 2..]).is_some() {
            out.push(hex_pair(&bytes[i + 2..]).unwrap_or(0));
            i += 4;
        } else {
            out.push(b'\\');
            i += 1;
        }
    }
    out
}

fn hex_pair(b: &[u8]) -> Option<u8> {
    let hex = std::str::from_utf8(b.get(..2)?).ok()?;
    u8::from_str_radix(hex, 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_control_keys() {
        assert_eq!(decode_keys(r"\x03"), [3]);
        assert_eq!(decode_keys(r"\x1b[A"), b"\x1b[A");
        assert_eq!(decode_keys(r"\e:wq\r"), b"\x1b:wq\r");
        assert_eq!(decode_keys(r"print(1)\n"), b"print(1)\n");
        assert_eq!(decode_keys("\u{3}"), [3]);
        assert_eq!(decode_keys(r"a\\nb"), br"a\nb");
    }

    #[test]
    fn leaves_unknown_and_incomplete_escapes_alone() {
        assert_eq!(decode_keys(r"C:\path"), br"C:\path");
        assert_eq!(decode_keys(r"\xZZ"), br"\xZZ");
        assert_eq!(decode_keys(r"\x4"), br"\x4");
        assert_eq!(decode_keys("end\\"), b"end\\");
        assert_eq!(decode_keys(""), b"");
    }
}
