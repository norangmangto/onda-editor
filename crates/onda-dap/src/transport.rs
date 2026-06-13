//! `Content-Length`-framed DAP transport over a debug-adapter subprocess (W15.1).
//!
//! Same framing as LSP; the reader task forwards parsed messages over a channel so
//! the main loop never blocks on adapter I/O (AGENTS.md rule 2).

use std::process::Stdio;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use serde_json::Value;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::mpsc;
use tracing::warn;

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("adapter process exited")]
    ProcessExited,
}

/// A parsed inbound DAP message.
#[derive(Debug, Clone)]
pub enum Incoming {
    Response(crate::protocol::Response),
    Event(crate::protocol::Event),
    /// A line/frame that didn't parse as a DAP message (isolated, never fatal).
    Malformed(String),
}

/// Classify a raw DAP JSON value by its `type` field.
pub fn classify(value: Value) -> Incoming {
    match value.get("type").and_then(|t| t.as_str()) {
        Some("response") => match serde_json::from_value(value.clone()) {
            Ok(r) => Incoming::Response(r),
            Err(_) => Incoming::Malformed(value.to_string()),
        },
        Some("event") => match serde_json::from_value(value.clone()) {
            Ok(e) => Incoming::Event(e),
            Err(_) => Incoming::Malformed(value.to_string()),
        },
        // "request" reverse-requests (e.g. runInTerminal) are not handled in v1.
        _ => Incoming::Malformed(value.to_string()),
    }
}

/// A spawned debug adapter with Content-Length framed I/O.
pub struct Transport {
    child: Child,
    stdin: ChildStdin,
    next_seq: Arc<AtomicI64>,
}

impl Transport {
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
                next_seq: Arc::new(AtomicI64::new(1)),
            },
            rx,
        ))
    }

    pub fn next_seq(&self) -> i64 {
        self.next_seq.fetch_add(1, Ordering::Relaxed)
    }

    /// Send a request; returns its `seq` so the caller can match the response.
    pub async fn send_request(
        &mut self,
        command: &str,
        arguments: Option<Value>,
    ) -> Result<i64, TransportError> {
        let seq = self.next_seq();
        let req = crate::protocol::Request {
            seq,
            kind: "request".into(),
            command: command.into(),
            arguments,
        };
        self.write(&serde_json::to_value(req)?).await?;
        Ok(seq)
    }

    async fn write(&mut self, value: &Value) -> Result<(), TransportError> {
        let body = serde_json::to_vec(value)?;
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        self.stdin.write_all(header.as_bytes()).await?;
        self.stdin.write_all(&body).await?;
        self.stdin.flush().await?;
        Ok(())
    }

    pub async fn shutdown(&mut self) {
        let _ = self.child.kill().await;
    }
}

async fn read_task(stdout: tokio::process::ChildStdout, tx: mpsc::Sender<Incoming>) {
    let mut reader = BufReader::new(stdout);
    loop {
        // Read headers until a blank line.
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
                    if let Some(rest) = line.strip_prefix("Content-Length:") {
                        if let Ok(n) = rest.trim().parse::<usize>() {
                            content_length = Some(n);
                        }
                    }
                }
                Err(e) => {
                    warn!("dap stdout read error: {e}");
                    return;
                }
            }
        }
        let len = match content_length {
            Some(n) => n,
            None => continue,
        };
        let mut buf = vec![0u8; len];
        if reader.read_exact(&mut buf).await.is_err() {
            return;
        }
        let msg = match serde_json::from_slice::<Value>(&buf) {
            Ok(v) => classify(v),
            Err(_) => Incoming::Malformed(String::from_utf8_lossy(&buf).into_owned()),
        };
        if tx.send(msg).await.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn classify_response_and_event() {
        let r = json!({"seq":2,"type":"response","request_seq":1,"success":true,"command":"initialize"});
        assert!(matches!(classify(r), Incoming::Response(_)));
        let e = json!({"seq":3,"type":"event","event":"stopped","body":{"reason":"breakpoint"}});
        assert!(matches!(classify(e), Incoming::Event(_)));
    }

    #[test]
    fn classify_garbage() {
        assert!(matches!(classify(json!({"x":1})), Incoming::Malformed(_)));
    }
}
