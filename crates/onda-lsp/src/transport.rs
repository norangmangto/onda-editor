/// JSON-RPC 2.0 transport over a process stdin/stdout.
///
/// Implements the LSP base protocol: Content-Length framed messages.
/// All I/O is non-blocking and managed by tokio tasks.
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::mpsc;
use tracing::{debug, warn};

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Channel send error")]
    ChannelClosed,
    #[error("Process exited unexpectedly")]
    ProcessExited,
}

// ── JSON-RPC message types ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: Option<Value>,
    pub method: String,
    pub params: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcNotification {
    pub jsonrpc: String,
    pub method: String,
    pub params: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

#[derive(Debug, Clone)]
pub enum IncomingMessage {
    Response(JsonRpcResponse),
    Notification(JsonRpcNotification),
}

// ── Transport ─────────────────────────────────────────────────────────────────

/// Manages a spawned language server process and its JSON-RPC I/O.
pub struct Transport {
    child: Child,
    stdin: ChildStdin,
    next_id: Arc<AtomicU64>,
}

impl Transport {
    /// Spawn a language server process.
    pub async fn spawn(
        command: &str,
        args: &[&str],
        root: &PathBuf,
    ) -> Result<(Self, mpsc::Receiver<IncomingMessage>), TransportError> {
        let mut child = Command::new(command)
            .args(args)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;

        let stdin = child.stdin.take().expect("stdin piped");
        let stdout = child.stdout.take().expect("stdout piped");

        let (tx, rx) = mpsc::channel(256);
        tokio::spawn(read_stdout_task(stdout, tx));

        Ok((
            Self {
                child,
                stdin,
                next_id: Arc::new(AtomicU64::new(1)),
            },
            rx,
        ))
    }

    /// Allocate a new unique request ID.
    pub fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Send a JSON-RPC request (with id) and return the id used.
    pub async fn send_request(
        &mut self,
        method: &str,
        params: Value,
    ) -> Result<u64, TransportError> {
        let id = self.next_id();
        let msg = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(Value::Number(id.into())),
            method: method.to_string(),
            params: Some(params),
        };
        self.write_message(&serde_json::to_value(msg)?).await?;
        Ok(id)
    }

    /// Send a JSON-RPC notification (no id).
    pub async fn send_notification(
        &mut self,
        method: &str,
        params: Value,
    ) -> Result<(), TransportError> {
        let msg = JsonRpcNotification {
            jsonrpc: "2.0".into(),
            method: method.to_string(),
            params: Some(params),
        };
        self.write_message(&serde_json::to_value(msg)?).await?;
        Ok(())
    }

    async fn write_message(&mut self, value: &Value) -> Result<(), TransportError> {
        let framed = encode_message(value)?;
        debug!(method = %value.get("method").and_then(|v| v.as_str()).unwrap_or("response"),
               len = framed.len(), "→ LSP");
        self.stdin.write_all(&framed).await?;
        self.stdin.flush().await?;
        Ok(())
    }

    /// Kill the server process.
    pub async fn shutdown(&mut self) {
        let _ = self.child.kill().await;
    }
}

// ── Framing helpers ─────────────────────────────────────────────────────────────

/// Frame a JSON value as a `Content-Length`-prefixed LSP message.
fn encode_message(value: &Value) -> Result<Vec<u8>, TransportError> {
    let body = serde_json::to_vec(value)?;
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(&body);
    Ok(out)
}

/// Read one `Content-Length`-framed message body from `reader`.
///
/// Returns `Ok(None)` on clean EOF. Non-`Content-Length` headers (e.g.
/// `Content-Type`) are ignored; a header block without a `Content-Length` is
/// skipped and the next block is tried.
async fn read_frame<R: AsyncBufRead + Unpin>(reader: &mut R) -> std::io::Result<Option<Vec<u8>>> {
    loop {
        let mut content_length: Option<usize> = None;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).await? == 0 {
                return Ok(None); // EOF
            }
            let line = line.trim_end();
            if line.is_empty() {
                break; // end of headers
            }
            if let Some(rest) = line.strip_prefix("Content-Length:") {
                if let Ok(n) = rest.trim().parse::<usize>() {
                    content_length = Some(n);
                }
            }
        }
        let Some(len) = content_length else {
            continue; // header block with no Content-Length — try the next
        };
        let mut body = vec![0u8; len];
        reader.read_exact(&mut body).await?;
        return Ok(Some(body));
    }
}

/// Parse a message body and classify it as a response or notification/request.
/// Returns `None` for malformed JSON (the caller skips it).
fn classify(body: &[u8]) -> Option<IncomingMessage> {
    let value: Value = serde_json::from_slice(body).ok()?;
    if value.get("id").is_some() && value.get("method").is_none() {
        serde_json::from_value::<JsonRpcResponse>(value)
            .ok()
            .map(IncomingMessage::Response)
    } else {
        serde_json::from_value::<JsonRpcNotification>(value)
            .ok()
            .map(IncomingMessage::Notification)
    }
}

// ── Stdout reader task ─────────────────────────────────────────────────────────

async fn read_stdout_task(stdout: ChildStdout, tx: mpsc::Sender<IncomingMessage>) {
    let mut reader = BufReader::new(stdout);
    loop {
        match read_frame(&mut reader).await {
            Ok(Some(body)) => {
                if let Some(msg) = classify(&body) {
                    if tx.send(msg).await.is_err() {
                        return;
                    }
                } else {
                    warn!("LSP: skipping unparseable message");
                }
            }
            Ok(None) => return, // EOF
            Err(e) => {
                warn!("LSP stdout read error: {e}");
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::BufReader;

    fn framed(body: &str) -> Vec<u8> {
        let mut v = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        v.extend_from_slice(body.as_bytes());
        v
    }

    #[tokio::test]
    async fn reads_a_response_frame() {
        let data = framed(r#"{"jsonrpc":"2.0","id":1,"result":{"ok":true}}"#);
        let mut r = BufReader::new(&data[..]);
        let body = read_frame(&mut r).await.unwrap().unwrap();
        assert!(matches!(
            classify(&body),
            Some(IncomingMessage::Response(_))
        ));
        // Next read hits EOF.
        assert!(read_frame(&mut r).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn reads_a_notification_frame() {
        let data = framed(r#"{"jsonrpc":"2.0","method":"window/logMessage","params":{}}"#);
        let mut r = BufReader::new(&data[..]);
        let body = read_frame(&mut r).await.unwrap().unwrap();
        match classify(&body) {
            Some(IncomingMessage::Notification(n)) => {
                assert_eq!(n.method, "window/logMessage")
            }
            other => panic!("expected notification, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn reads_two_back_to_back_frames() {
        let mut data = framed(r#"{"jsonrpc":"2.0","id":1,"result":1}"#);
        data.extend(framed(r#"{"jsonrpc":"2.0","method":"x","params":null}"#));
        let mut r = BufReader::new(&data[..]);
        let a = read_frame(&mut r).await.unwrap().unwrap();
        let b = read_frame(&mut r).await.unwrap().unwrap();
        assert!(matches!(classify(&a), Some(IncomingMessage::Response(_))));
        assert!(matches!(
            classify(&b),
            Some(IncomingMessage::Notification(_))
        ));
    }

    #[tokio::test]
    async fn ignores_extra_headers() {
        let body = r#"{"jsonrpc":"2.0","id":2,"result":null}"#;
        let mut data = format!(
            "Content-Type: application/vscode-jsonrpc\r\nContent-Length: {}\r\n\r\n",
            body.len()
        )
        .into_bytes();
        data.extend_from_slice(body.as_bytes());
        let mut r = BufReader::new(&data[..]);
        let got = read_frame(&mut r).await.unwrap().unwrap();
        assert_eq!(got, body.as_bytes());
    }

    #[tokio::test]
    async fn encode_then_read_round_trips() {
        let value = serde_json::json!({"jsonrpc":"2.0","id":7,"result":"ok"});
        let bytes = encode_message(&value).unwrap();
        let mut r = BufReader::new(&bytes[..]);
        let body = read_frame(&mut r).await.unwrap().unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&body).unwrap(), value);
    }

    #[test]
    fn classify_rejects_malformed_json() {
        assert!(classify(b"{ not json").is_none());
    }
}
