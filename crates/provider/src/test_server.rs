//! A one-request HTTP server for adapter tests.

use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Serves one response and returns the server's base URL and a handle that
/// yields the raw request it received.
pub(crate) async fn serve_once(
    status: &'static str,
    content_type: &'static str,
    body: String,
) -> (String, tokio::task::JoinHandle<String>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let handle = tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut req = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            let n = sock.read(&mut buf).await.unwrap();
            req.extend_from_slice(&buf[..n]);
            let text = String::from_utf8_lossy(&req).to_string();
            if let Some(h) = text.find("\r\n\r\n") {
                let len: usize = text[..h]
                    .lines()
                    .find_map(|l| {
                        l.to_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse().unwrap())
                    })
                    .unwrap_or(0);
                if req.len() >= h + 4 + len {
                    break;
                }
            }
            if n == 0 {
                break;
            }
        }
        let resp = format!(
            "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\nconnection: close\r\n\r\n{body}"
        );
        sock.write_all(resp.as_bytes()).await.unwrap();
        sock.shutdown().await.unwrap();
        String::from_utf8_lossy(&req).into_owned()
    });
    (url, handle)
}

/// The JSON body of a raw HTTP request.
pub(crate) fn body_of(raw: &str) -> serde_json::Value {
    serde_json::from_str(&raw[raw.find("\r\n\r\n").unwrap() + 4..]).unwrap()
}
