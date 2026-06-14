//! ACP agent client for onda (Phase 4 W22).
//!
//! `onda-agent` speaks the Agent Client Protocol (vendored in [`protocol`]) to an
//! external agent subprocess over NDJSON JSON-RPC ([`transport`]). The protocol
//! state machine ([`session`]) is pure and unit-tested; [`AgentClient`] wires it to
//! a tokio driver task that the editor talks to over channels — the main loop never
//! blocks on agent I/O (AGENTS.md rule 2).

pub mod mentions;
pub mod permissions;
pub mod protocol;
pub mod session;
pub mod staging;
pub mod transport;

use std::path::PathBuf;

use serde_json::{json, Value};
use thiserror::Error;
use tokio::sync::mpsc;
use tracing::{debug, warn};

pub use mentions::{
    build_context, format_diagnostics, parse_mentions, DiagnosticItem, Mention, MentionKind,
    ResolvedContext, Severity,
};
pub use permissions::{Decision, PermissionStore, Rule, Scope, Target};
pub use protocol::{
    ContentBlock, PermissionOption, PermissionOptionKind, PermissionOutcome, PlanEntry,
    ReadTextFileParams, RequestPermissionParams, StopReason, ToolCall, ToolCallStatus,
    ToolCallUpdate, ToolKind,
};
pub use session::{AgentEvent, PendingKind, SessionState};
pub use staging::{
    apply_selected, file_hunks, hunk_removed, Hunk, ProposedEdit, Resolution, StagingArea,
};
use transport::{Incoming, JsonRpcError, Transport, TransportError};

#[derive(Debug, Error)]
pub enum AgentError {
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error("agent driver channel closed")]
    Closed,
}

/// A configured agent the user can connect to (`:agent <name>`).
#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// Named registry of configured agents.
#[derive(Debug, Default, Clone)]
pub struct AgentRegistry {
    agents: Vec<AgentConfig>,
}

impl AgentRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, cfg: AgentConfig) {
        self.agents.retain(|a| a.name != cfg.name);
        self.agents.push(cfg);
    }

    pub fn get(&self, name: &str) -> Option<&AgentConfig> {
        self.agents.iter().find(|a| a.name == name)
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.agents.iter().map(|a| a.name.as_str())
    }
}

/// Commands the editor sends to the agent driver task.
#[derive(Debug)]
pub enum AgentCommand {
    /// Send a user prompt (content blocks: text + resource mentions).
    Prompt(Vec<ContentBlock>),
    /// Cancel the in-flight turn.
    Cancel,
    /// Reply to a `session/request_permission`.
    RespondPermission {
        id: Value,
        outcome: PermissionOutcome,
    },
    /// Reply to an `fs/read_text_file` (Ok(content) served from buffer state).
    RespondFileRead {
        id: Value,
        content: Result<String, String>,
    },
    /// Reply to an `fs/write_text_file` (Ok once staged, or Err to reject).
    RespondFileWrite {
        id: Value,
        result: Result<(), String>,
    },
    /// Reply to an unrecognized agent request with method-not-found.
    RespondUnknown { id: Value },
    /// Tear down the session and the subprocess.
    Shutdown,
}

/// Handle the editor uses to talk to a connected agent.
pub struct AgentClient {
    cmd_tx: mpsc::Sender<AgentCommand>,
}

impl AgentClient {
    /// Spawn the agent and start the driver. The driver auto-runs the handshake
    /// (`initialize` → `session/new`) and forwards [`AgentEvent`]s on `events`.
    pub async fn connect(
        cfg: &AgentConfig,
        cwd: PathBuf,
        events: mpsc::Sender<AgentEvent>,
    ) -> Result<Self, AgentError> {
        let (mut transport, rx) = Transport::spawn(&cfg.command, &cfg.args, &cwd, &cfg.env).await?;
        let mut session = SessionState::new();

        // Kick off the handshake: initialize first.
        let init_params = json!({
            "protocolVersion": protocol::PROTOCOL_VERSION,
            "clientCapabilities": { "fs": { "readTextFile": true, "writeTextFile": true } }
        });
        let id = transport
            .send_request(protocol::method::INITIALIZE, init_params)
            .await?;
        session.expect_response(id, PendingKind::Initialize);

        let (cmd_tx, cmd_rx) = mpsc::channel(64);
        tokio::spawn(driver(transport, rx, session, cmd_rx, events, cwd));
        Ok(Self { cmd_tx })
    }

    async fn send(&self, cmd: AgentCommand) -> Result<(), AgentError> {
        self.cmd_tx.send(cmd).await.map_err(|_| AgentError::Closed)
    }

    /// Non-blocking dispatch for callers outside an async context (the editor's
    /// synchronous main loop). Returns false if the driver is gone or its queue full.
    pub fn dispatch(&self, cmd: AgentCommand) -> bool {
        self.cmd_tx.try_send(cmd).is_ok()
    }

    pub async fn prompt(&self, blocks: Vec<ContentBlock>) -> Result<(), AgentError> {
        self.send(AgentCommand::Prompt(blocks)).await
    }
    pub async fn cancel(&self) -> Result<(), AgentError> {
        self.send(AgentCommand::Cancel).await
    }
    pub async fn respond_permission(
        &self,
        id: Value,
        outcome: PermissionOutcome,
    ) -> Result<(), AgentError> {
        self.send(AgentCommand::RespondPermission { id, outcome })
            .await
    }
    pub async fn respond_file_read(
        &self,
        id: Value,
        content: Result<String, String>,
    ) -> Result<(), AgentError> {
        self.send(AgentCommand::RespondFileRead { id, content })
            .await
    }
    pub async fn respond_file_write(
        &self,
        id: Value,
        result: Result<(), String>,
    ) -> Result<(), AgentError> {
        self.send(AgentCommand::RespondFileWrite { id, result })
            .await
    }
    pub async fn respond_unknown(&self, id: Value) -> Result<(), AgentError> {
        self.send(AgentCommand::RespondUnknown { id }).await
    }
    pub async fn shutdown(&self) -> Result<(), AgentError> {
        self.send(AgentCommand::Shutdown).await
    }
}

/// The driver task: owns the transport + session, pumps inbound messages into
/// events, and applies editor commands. Auto-advances the handshake.
async fn driver(
    mut transport: Transport,
    mut rx: mpsc::Receiver<Incoming>,
    mut session: SessionState,
    mut cmd_rx: mpsc::Receiver<AgentCommand>,
    events: mpsc::Sender<AgentEvent>,
    cwd: PathBuf,
) {
    loop {
        tokio::select! {
            inbound = rx.recv() => {
                match inbound {
                    Some(msg) => {
                        for ev in session.process(msg) {
                            // Drive the handshake internally on Initialized.
                            if let AgentEvent::Initialized { .. } = ev {
                                let params = json!({ "cwd": cwd.to_string_lossy(), "mcpServers": [] });
                                match transport.send_request(protocol::method::SESSION_NEW, params).await {
                                    Ok(id) => session.expect_response(id, PendingKind::NewSession),
                                    Err(e) => warn!("session/new send failed: {e}"),
                                }
                            }
                            if events.send(ev).await.is_err() {
                                transport.shutdown().await;
                                return;
                            }
                        }
                    }
                    None => {
                        // Agent closed stdout: disconnected.
                        let _ = events.send(AgentEvent::Error {
                            message: "agent disconnected".into(),
                        }).await;
                        transport.shutdown().await;
                        return;
                    }
                }
            }
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(AgentCommand::Prompt(blocks)) => {
                        let Some(sid) = session.session_id.clone() else {
                            let _ = events.send(AgentEvent::Error {
                                message: "no active session yet".into(),
                            }).await;
                            continue;
                        };
                        let params = json!({ "sessionId": sid, "prompt": blocks });
                        match transport.send_request(protocol::method::SESSION_PROMPT, params).await {
                            Ok(id) => session.expect_response(id, PendingKind::Prompt),
                            Err(e) => warn!("session/prompt send failed: {e}"),
                        }
                    }
                    Some(AgentCommand::Cancel) => {
                        if let Some(sid) = session.session_id.clone() {
                            let _ = transport.send_notification(
                                protocol::method::SESSION_CANCEL,
                                json!({ "sessionId": sid }),
                            ).await;
                        }
                    }
                    Some(AgentCommand::RespondPermission { id, outcome }) => {
                        let _ = transport.send_response(id, Ok(json!({ "outcome": outcome }))).await;
                    }
                    Some(AgentCommand::RespondFileRead { id, content }) => {
                        let resp = match content {
                            Ok(c) => Ok(json!({ "content": c })),
                            Err(e) => Err(JsonRpcError { code: -32000, message: e, data: None }),
                        };
                        let _ = transport.send_response(id, resp).await;
                    }
                    Some(AgentCommand::RespondFileWrite { id, result }) => {
                        let resp = match result {
                            Ok(()) => Ok(Value::Null),
                            Err(e) => Err(JsonRpcError { code: -32001, message: e, data: None }),
                        };
                        let _ = transport.send_response(id, resp).await;
                    }
                    Some(AgentCommand::RespondUnknown { id }) => {
                        let _ = transport.send_response(id, Err(JsonRpcError {
                            code: -32601, message: "method not found".into(), data: None,
                        })).await;
                    }
                    Some(AgentCommand::Shutdown) | None => {
                        debug!("agent driver shutting down");
                        transport.shutdown().await;
                        return;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_add_get_replace() {
        let mut reg = AgentRegistry::new();
        reg.add(AgentConfig {
            name: "claude".into(),
            command: "claude-code".into(),
            args: vec!["acp".into()],
            env: vec![],
        });
        assert!(reg.get("claude").is_some());
        // Re-adding the same name replaces, not duplicates.
        reg.add(AgentConfig {
            name: "claude".into(),
            command: "claude2".into(),
            args: vec![],
            env: vec![],
        });
        assert_eq!(reg.get("claude").unwrap().command, "claude2");
        assert_eq!(reg.names().count(), 1);
    }
}
