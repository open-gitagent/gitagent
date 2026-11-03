use std::path::Path;

use anyhow::Result;
use gitagent_provider::{ImageData, ToolCall};

use crate::{str_arg, ToolOutput};

/// Providers reject larger images (about 5 MB once base64-encoded).
const MAX_IMAGE_BYTES: usize = 3_700_000;

pub(crate) fn run(call: &ToolCall, cwd: &Path) -> Result<ToolOutput> {
    let path = cwd.join(str_arg(call, "path")?);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            return Ok(ToolOutput::error(format!(
                "Cannot read {}: {e}",
                path.display()
            )))
        }
    };
    let Some(media_type) = media_type(&bytes) else {
        return Ok(ToolOutput::error(format!(
            "{} is not a PNG, JPEG, GIF or WebP image. Convert it first (e.g. with ImageMagick or Python).",
            path.display()
        )));
    };
    if bytes.len() > MAX_IMAGE_BYTES {
        return Ok(ToolOutput::error(format!(
            "{} is {} bytes; images over {} bytes are rejected. Scale it down first.",
            path.display(),
            bytes.len(),
            MAX_IMAGE_BYTES
        )));
    }
    let mut out = ToolOutput::ok(format!(
        "Image {} ({media_type}, {} bytes).",
        path.display(),
        bytes.len()
    ));
    out.images.push(ImageData {
        media_type: media_type.to_string(),
        data: base64(&bytes),
    });
    Ok(out)
}

/// Detects the format from the file's first bytes.
fn media_type(b: &[u8]) -> Option<&'static str> {
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if b.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if b.len() >= 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// Standard base64 with padding.
fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(path: &str) -> ToolCall {
        ToolCall {
            id: "c".into(),
            name: "image_read".into(),
            arguments: json!({"path": path}).as_object().unwrap().clone(),
        }
    }

    #[test]
    fn base64_matches_the_standard_alphabet_and_padding() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(&[0xff, 0xfe, 0xfd]), "//79");
    }

    #[test]
    fn reads_images_and_rejects_other_files() {
        let dir = tempfile::tempdir().unwrap();
        let png = b"\x89PNG\r\n\x1a\nrest-of-file";
        std::fs::write(dir.path().join("a.png"), png).unwrap();
        let out = run(&call("a.png"), dir.path()).unwrap();
        assert!(!out.is_error);
        assert_eq!(out.images[0].media_type, "image/png");
        assert_eq!(out.images[0].data, base64(png));

        std::fs::write(dir.path().join("b.txt"), "hello").unwrap();
        assert!(run(&call("b.txt"), dir.path()).unwrap().is_error);
        assert!(run(&call("missing.png"), dir.path()).unwrap().is_error);
        assert_eq!(media_type(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
    }
}
