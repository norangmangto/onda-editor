//! Newline-delimited JSON-RPC 2.0 transport over a subprocess's stdio (T22.1).
//!
//! ACP frames each JSON-RPC message as one line (NDJSON), unlike LSP's
//! `Content-Length` framing. The reader task forwards parsed messages over a
//! channel; the main loop never blocks on agent I/O (AGENTS.md rule 2).

use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::mpsc;
use tracing::warn;

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("agent process exited")]
    ProcessExited,
}

// ── JSON-RPC envelopes ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: Value,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcNotification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// A parsed inbound message from the agent.
#[derive(Debug, Clone)]
pub enum Incoming {
    /// A response to a request the client sent.
    Response(JsonRpcResponse),
    /// A notification from the agent (no id).
    Notification(JsonRpcNotification),
    /// A request *from* the agent (has id + method) — client must respond.
    Request(JsonRpcRequest),
    /// A line that could not be parsed as JSON-RPC (isolated, never fatal).
    Malformed(String),
}

/// Classify a raw JSON value into an `Incoming` variant.
pub fn classify(value: Value) -> Incoming {
    let has_id = value.get("id").map(|v| !v.is_null()).unwrap_or(false);
    let has_method = value.get("method").is_some();
    match (has_id, has_method) {
        (true, true) => match serde_json::from_value::<JsonRpcRequest>(value.clone()) {
            Ok(r) => Incoming::Request(r),
            Err(_) => Incoming::Malformed(value.to_string()),
        },
        (true, false) => match serde_json::from_value::<JsonRpcResponse>(value.clone()) {
            Ok(r) => Incoming::Response(r),
            Err(_) => Incoming::Malformed(value.to_string()),
        },
        (false, true) => match serde_json::from_value::<JsonRpcNotification>(value.clone()) {
            Ok(n) => Incoming::Notification(n),
            Err(_) => Incoming::Malformed(value.to_string()),
        },
        (false, false) => Incoming::Malformed(value.to_string()),
    }
}

/// Encode one outbound message as an NDJSON line (including the trailing `\n`).
pub fn encode_line(value: &Value) -> Result<Vec<u8>, TransportError> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    Ok(bytes)
}

// ── Transport ───────────────────────────────────────────────────────────────────

/// A spawned agent subprocess with NDJSON JSON-RPC I/O.
pub struct Transport {
    child: Child,
    stdin: ChildStdin,
    next_id: Arc<AtomicU64>,
}

impl Transport {
    /// Spawn `command args...` in `cwd`; returns the transport and a receiver of
    /// inbound messages.
    pub async fn spawn(
        command: &str,
        args: &[String],
        cwd: &std::path::Path,
        env: &[(String, String)],
    ) -> Result<(Self, mpsc::Receiver<Incoming>), TransportError> {
        let mut cmd = Command::new(command);
        cmd.args(args)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take().ok_or(TransportError::ProcessExited)?;
        let stdout = child.stdout.take().ok_or(TransportError::ProcessExited)?;

        let (tx, rx) = mpsc::channel(256);
        tokio::spawn(read_task(stdout, tx));

        Ok((
            Self {
                child,
                stdin,
                next_id: Arc::new(AtomicU64::new(1)),
            },
            rx,
        ))
    }

    pub fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Send a request; returns the id used so the caller can match the response.
    pub async fn send_request(
        &mut self,
        method: &str,
        params: Value,
    ) -> Result<u64, TransportError> {
        let id = self.next_id();
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Value::from(id),
            method: method.into(),
            params: Some(params),
        };
        self.write(&serde_json::to_value(req)?).await?;
        Ok(id)
    }

    pub async fn send_notification(
        &mut self,
        method: &str,
        params: Value,
    ) -> Result<(), TransportError> {
        let note = JsonRpcNotification {
            jsonrpc: "2.0".into(),
            method: method.into(),
            params: Some(params),
        };
        self.write(&serde_json::to_value(note)?).await
    }

    /// Respond to a request the agent sent us.
    pub async fn send_response(
        &mut self,
        id: Value,
        result: Result<Value, JsonRpcError>,
    ) -> Result<(), TransportError> {
        let resp = match result {
            Ok(v) => JsonRpcResponse {
                jsonrpc: "2.0".into(),
                id,
                result: Some(v),
                error: None,
            },
            Err(e) => JsonRpcResponse {
                jsonrpc: "2.0".into(),
                id,
                result: None,
                error: Some(e),
            },
        };
        self.write(&serde_json::to_value(resp)?).await
    }

    async fn write(&mut self, value: &Value) -> Result<(), TransportError> {
        let line = encode_line(value)?;
        self.stdin.write_all(&line).await?;
        self.stdin.flush().await?;
        Ok(())
    }

    pub async fn shutdown(&mut self) {
        let _ = self.child.kill().await;
    }
}

async fn read_task(stdout: tokio::process::ChildStdout, tx: mpsc::Sender<Incoming>) {
    let mut lines = BufReader::new(stdout).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let msg = match serde_json::from_str::<Value>(trimmed) {
                    Ok(v) => classify(v),
                    Err(_) => Incoming::Malformed(trimmed.to_string()),
                };
                if tx.send(msg).await.is_err() {
                    return;
                }
            }
            Ok(None) => return, // EOF: agent closed stdout
            Err(e) => {
                warn!("agent stdout read error: {e}");
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn classify_response() {
        let v = json!({"jsonrpc":"2.0","id":1,"result":{"ok":true}});
        assert!(matches!(classify(v), Incoming::Response(_)));
    }

    #[test]
    fn classify_notification() {
        let v = json!({"jsonrpc":"2.0","method":"session/update","params":{}});
        assert!(matches!(classify(v), Incoming::Notification(_)));
    }

    #[test]
    fn classify_request_from_agent() {
        let v = json!({"jsonrpc":"2.0","id":7,"method":"fs/read_text_file","params":{}});
        match classify(v) {
            Incoming::Request(r) => assert_eq!(r.method, "fs/read_text_file"),
            other => panic!("expected request, got {other:?}"),
        }
    }

    #[test]
    fn classify_garbage_is_malformed() {
        let v = json!({"nonsense": true});
        assert!(matches!(classify(v), Incoming::Malformed(_)));
    }

    #[test]
    fn encode_line_appends_newline() {
        let line = encode_line(&json!({"a":1})).unwrap();
        assert_eq!(*line.last().unwrap(), b'\n');
        assert!(!line[..line.len() - 1].contains(&b'\n'));
    }
}
