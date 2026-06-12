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
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
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
        let body = serde_json::to_vec(value)?;
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        debug!(method = %value.get("method").and_then(|v| v.as_str()).unwrap_or("response"),
               len = body.len(), "→ LSP");
        self.stdin.write_all(header.as_bytes()).await?;
        self.stdin.write_all(&body).await?;
        self.stdin.flush().await?;
        Ok(())
    }

    /// Kill the server process.
    pub async fn shutdown(&mut self) {
        let _ = self.child.kill().await;
    }
}

// ── Stdout reader task ─────────────────────────────────────────────────────────

async fn read_stdout_task(stdout: ChildStdout, tx: mpsc::Sender<IncomingMessage>) {
    let mut reader = BufReader::new(stdout);
    loop {
        // Read headers until blank line
        let mut content_length: Option<usize> = None;
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line).await {
                Ok(0) => return, // EOF
                Ok(_) => {
                    let line = line.trim();
                    if line.is_empty() {
                        break;
                    }
                    if let Some(rest) = line.strip_prefix("Content-Length: ") {
                        if let Ok(n) = rest.parse::<usize>() {
                            content_length = Some(n);
                        }
                    }
                }
                Err(e) => {
                    warn!("LSP stdout read error: {e}");
                    return;
                }
            }
        }

        let len = match content_length {
            Some(n) => n,
            None => {
                warn!("LSP: missing Content-Length header");
                continue;
            }
        };

        let mut body = vec![0u8; len];
        if let Err(e) = reader.read_exact(&mut body).await {
            warn!("LSP body read error: {e}");
            return;
        }

        let value: Value = match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(e) => {
                warn!("LSP JSON parse error: {e}");
                continue;
            }
        };

        debug!(method = %value.get("method").and_then(|v| v.as_str()).unwrap_or("response"),
               "← LSP");

        let msg = if value.get("id").is_some() && value.get("method").is_none() {
            // Response
            match serde_json::from_value::<JsonRpcResponse>(value) {
                Ok(r) => IncomingMessage::Response(r),
                Err(e) => {
                    warn!("LSP response parse error: {e}");
                    continue;
                }
            }
        } else {
            // Notification or request from server
            match serde_json::from_value::<JsonRpcNotification>(value) {
                Ok(n) => IncomingMessage::Notification(n),
                Err(e) => {
                    warn!("LSP notification parse error: {e}");
                    continue;
                }
            }
        };

        if tx.send(msg).await.is_err() {
            return;
        }
    }
}
