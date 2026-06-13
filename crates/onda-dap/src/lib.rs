//! DAP debugger client for onda (Phase 3 W15).
//!
//! `onda-dap` speaks the Debug Adapter Protocol (vendored in [`protocol`]) to an
//! adapter subprocess (`lldb-dap`, `debugpy`, …) over Content-Length framed stdio
//! ([`transport`]). The protocol state machine ([`session`]) is pure and unit-tested;
//! [`DapClient`] wires it to a tokio driver task the editor talks to over channels —
//! the main loop never blocks on adapter I/O (AGENTS.md rule 2).

pub mod protocol;
pub mod session;
pub mod transport;

use std::collections::HashMap;
use std::path::PathBuf;

use serde_json::{json, Value};
use thiserror::Error;
use tokio::sync::mpsc;
use tracing::warn;

pub use protocol::{Breakpoint, Scope, SourceBreakpoint, StackFrame, Thread, Variable};
pub use session::{DapEvent, DapSession, PendingKind};
use transport::{Incoming, Transport, TransportError};

#[derive(Debug, Error)]
pub enum DapError {
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error("dap driver channel closed")]
    Closed,
}

/// A configured debug adapter (from `dap.toml`).
#[derive(Debug, Clone)]
pub struct AdapterConfig {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    /// Language names this adapter handles (e.g. `["rust", "c"]`).
    pub languages: Vec<String>,
    /// The `launch` request arguments (e.g. `{ "program": "..." }`).
    pub launch: Value,
}

/// Named registry of debug adapters.
#[derive(Debug, Default, Clone)]
pub struct DapRegistry {
    adapters: Vec<AdapterConfig>,
}

impl DapRegistry {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn add(&mut self, cfg: AdapterConfig) {
        self.adapters.retain(|a| a.name != cfg.name);
        self.adapters.push(cfg);
    }
    pub fn by_name(&self, name: &str) -> Option<&AdapterConfig> {
        self.adapters.iter().find(|a| a.name == name)
    }
    /// First adapter that handles `language`.
    pub fn for_language(&self, language: &str) -> Option<&AdapterConfig> {
        self.adapters
            .iter()
            .find(|a| a.languages.iter().any(|l| l == language))
    }
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.adapters.iter().map(|a| a.name.as_str())
    }
}

/// Commands the editor sends to the DAP driver task.
#[derive(Debug)]
pub enum DapCommand {
    /// Replace breakpoints for `path` (sent now if configured, else at config time).
    SetBreakpoints {
        path: PathBuf,
        breakpoints: Vec<SourceBreakpoint>,
    },
    Continue {
        thread_id: i64,
    },
    Next {
        thread_id: i64,
    },
    StepIn {
        thread_id: i64,
    },
    StepOut {
        thread_id: i64,
    },
    Threads,
    StackTrace {
        thread_id: i64,
    },
    Scopes {
        frame_id: i64,
    },
    Variables {
        variables_reference: i64,
    },
    Evaluate {
        expression: String,
        frame_id: Option<i64>,
    },
    Disconnect,
}

/// Handle the editor uses to drive a debug session.
pub struct DapClient {
    cmd_tx: mpsc::Sender<DapCommand>,
}

impl DapClient {
    /// Spawn the adapter and start the driver. Runs the handshake automatically
    /// (initialize → launch → on `initialized`: setBreakpoints + configurationDone).
    pub async fn launch(
        cfg: &AdapterConfig,
        cwd: PathBuf,
        events: mpsc::Sender<DapEvent>,
    ) -> Result<Self, DapError> {
        let (mut transport, rx) = Transport::spawn(&cfg.command, &cfg.args, &cwd, &cfg.env).await?;
        let mut session = DapSession::new();

        let init = serde_json::to_value(protocol::InitializeArgs::default()).ok();
        let seq = transport
            .send_request(protocol::command::INITIALIZE, init)
            .await?;
        session.expect(seq, PendingKind::Initialize);

        let (cmd_tx, cmd_rx) = mpsc::channel(64);
        tokio::spawn(driver(
            transport,
            rx,
            session,
            cmd_rx,
            events,
            cfg.launch.clone(),
        ));
        Ok(Self { cmd_tx })
    }

    async fn send(&self, cmd: DapCommand) -> Result<(), DapError> {
        self.cmd_tx.send(cmd).await.map_err(|_| DapError::Closed)
    }

    pub async fn set_breakpoints(
        &self,
        path: PathBuf,
        breakpoints: Vec<SourceBreakpoint>,
    ) -> Result<(), DapError> {
        self.send(DapCommand::SetBreakpoints { path, breakpoints })
            .await
    }
    pub async fn continue_(&self, thread_id: i64) -> Result<(), DapError> {
        self.send(DapCommand::Continue { thread_id }).await
    }
    pub async fn next(&self, thread_id: i64) -> Result<(), DapError> {
        self.send(DapCommand::Next { thread_id }).await
    }
    pub async fn step_in(&self, thread_id: i64) -> Result<(), DapError> {
        self.send(DapCommand::StepIn { thread_id }).await
    }
    pub async fn step_out(&self, thread_id: i64) -> Result<(), DapError> {
        self.send(DapCommand::StepOut { thread_id }).await
    }
    pub async fn threads(&self) -> Result<(), DapError> {
        self.send(DapCommand::Threads).await
    }
    pub async fn stack_trace(&self, thread_id: i64) -> Result<(), DapError> {
        self.send(DapCommand::StackTrace { thread_id }).await
    }
    pub async fn scopes(&self, frame_id: i64) -> Result<(), DapError> {
        self.send(DapCommand::Scopes { frame_id }).await
    }
    pub async fn variables(&self, variables_reference: i64) -> Result<(), DapError> {
        self.send(DapCommand::Variables {
            variables_reference,
        })
        .await
    }
    pub async fn evaluate(
        &self,
        expression: String,
        frame_id: Option<i64>,
    ) -> Result<(), DapError> {
        self.send(DapCommand::Evaluate {
            expression,
            frame_id,
        })
        .await
    }
    pub async fn disconnect(&self) -> Result<(), DapError> {
        self.send(DapCommand::Disconnect).await
    }
}

async fn driver(
    mut transport: Transport,
    mut rx: mpsc::Receiver<Incoming>,
    mut session: DapSession,
    mut cmd_rx: mpsc::Receiver<DapCommand>,
    events: mpsc::Sender<DapEvent>,
    launch_args: Value,
) {
    // Breakpoints accumulate before the config phase, then are flushed on `initialized`.
    let mut breakpoints: HashMap<PathBuf, Vec<SourceBreakpoint>> = HashMap::new();
    let mut configured = false;

    loop {
        tokio::select! {
            inbound = rx.recv() => {
                let Some(msg) = inbound else {
                    let _ = events.send(DapEvent::Error("adapter disconnected".into())).await;
                    transport.shutdown().await;
                    return;
                };
                for ev in session.process(msg) {
                    match &ev {
                        DapEvent::AdapterReady => {
                            let seq = transport
                                .send_request(protocol::command::LAUNCH, Some(launch_args.clone()))
                                .await;
                            if let Ok(seq) = seq { session.expect(seq, PendingKind::Launch); }
                        }
                        DapEvent::ConfigReady => {
                            for (path, bps) in &breakpoints {
                                send_breakpoints(&mut transport, &mut session, path, bps).await;
                            }
                            if let Ok(seq) = transport
                                .send_request(protocol::command::CONFIGURATION_DONE, None)
                                .await
                            {
                                session.expect(seq, PendingKind::ConfigurationDone);
                            }
                            configured = true;
                        }
                        _ => {}
                    }
                    if events.send(ev).await.is_err() {
                        transport.shutdown().await;
                        return;
                    }
                }
            }
            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else { transport.shutdown().await; return; };
                match cmd {
                    DapCommand::SetBreakpoints { path, breakpoints: bps } => {
                        breakpoints.insert(path.clone(), bps.clone());
                        if configured {
                            send_breakpoints(&mut transport, &mut session, &path, &bps).await;
                        }
                    }
                    DapCommand::Continue { thread_id } => {
                        req(&mut transport, &mut session, protocol::command::CONTINUE,
                            json!({ "threadId": thread_id }), PendingKind::Continue).await;
                    }
                    DapCommand::Next { thread_id } => {
                        req(&mut transport, &mut session, protocol::command::NEXT,
                            json!({ "threadId": thread_id }), PendingKind::Step).await;
                    }
                    DapCommand::StepIn { thread_id } => {
                        req(&mut transport, &mut session, protocol::command::STEP_IN,
                            json!({ "threadId": thread_id }), PendingKind::Step).await;
                    }
                    DapCommand::StepOut { thread_id } => {
                        req(&mut transport, &mut session, protocol::command::STEP_OUT,
                            json!({ "threadId": thread_id }), PendingKind::Step).await;
                    }
                    DapCommand::Threads => {
                        req(&mut transport, &mut session, protocol::command::THREADS,
                            Value::Null, PendingKind::Threads).await;
                    }
                    DapCommand::StackTrace { thread_id } => {
                        req(&mut transport, &mut session, protocol::command::STACK_TRACE,
                            json!({ "threadId": thread_id }), PendingKind::StackTrace).await;
                    }
                    DapCommand::Scopes { frame_id } => {
                        req(&mut transport, &mut session, protocol::command::SCOPES,
                            json!({ "frameId": frame_id }), PendingKind::Scopes).await;
                    }
                    DapCommand::Variables { variables_reference } => {
                        req(&mut transport, &mut session, protocol::command::VARIABLES,
                            json!({ "variablesReference": variables_reference }), PendingKind::Variables).await;
                    }
                    DapCommand::Evaluate { expression, frame_id } => {
                        req(&mut transport, &mut session, protocol::command::EVALUATE,
                            json!({ "expression": expression, "frameId": frame_id, "context": "repl" }),
                            PendingKind::Evaluate).await;
                    }
                    DapCommand::Disconnect => {
                        req(&mut transport, &mut session, protocol::command::DISCONNECT,
                            json!({ "terminateDebuggee": true }), PendingKind::Disconnect).await;
                        transport.shutdown().await;
                        return;
                    }
                }
            }
        }
    }
}

async fn send_breakpoints(
    transport: &mut Transport,
    session: &mut DapSession,
    path: &std::path::Path,
    bps: &[SourceBreakpoint],
) {
    let args = json!({
        "source": { "path": path.to_string_lossy(), "name": path.file_name().and_then(|n| n.to_str()) },
        "breakpoints": bps,
    });
    if let Ok(seq) = transport
        .send_request(protocol::command::SET_BREAKPOINTS, Some(args))
        .await
    {
        session.expect(seq, PendingKind::SetBreakpoints);
    }
}

async fn req(
    transport: &mut Transport,
    session: &mut DapSession,
    command: &str,
    args: Value,
    kind: PendingKind,
) {
    let args = if args.is_null() { None } else { Some(args) };
    match transport.send_request(command, args).await {
        Ok(seq) => session.expect(seq, kind),
        Err(e) => warn!("dap send {command} failed: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_resolves_by_language() {
        let mut reg = DapRegistry::new();
        reg.add(AdapterConfig {
            name: "lldb".into(),
            command: "lldb-dap".into(),
            args: vec![],
            env: vec![],
            languages: vec!["rust".into(), "c".into()],
            launch: json!({"program":"a.out"}),
        });
        assert!(reg.for_language("rust").is_some());
        assert!(reg.for_language("c").is_some());
        assert!(reg.for_language("python").is_none());
        assert_eq!(reg.by_name("lldb").unwrap().command, "lldb-dap");
    }
}
