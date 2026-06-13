//! ACP session state machine (T22.1/T22.2).
//!
//! Pure logic: feed inbound [`Incoming`] messages, get back [`AgentEvent`]s the
//! editor/panel consumes. No I/O lives here, so the full protocol surface —
//! streaming, tool calls, permission/file requests, cancellation, unknown-message
//! isolation — is unit-testable without spawning a process.

use serde_json::Value;

use crate::protocol::*;
use crate::transport::{Incoming, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse};

/// What a client-sent request was, so the matching response can be routed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingKind {
    Initialize,
    NewSession,
    Prompt,
}

/// Events surfaced to the editor from the agent stream.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentEvent {
    Initialized {
        protocol_version: u32,
    },
    SessionCreated {
        session_id: String,
    },
    /// Streaming assistant text (append to the panel tail).
    MessageChunk {
        text: String,
    },
    /// Streaming agent "thinking" text.
    ThoughtChunk {
        text: String,
    },
    ToolCallStarted(ToolCall),
    ToolCallUpdated(ToolCallUpdate),
    Plan(Vec<PlanEntry>),
    /// Agent asks permission for a tool call; reply via the carried request id.
    PermissionRequest {
        request_id: Value,
        params: RequestPermissionParams,
    },
    /// Agent wants to read a file; serve from buffer state and reply.
    FileReadRequest {
        request_id: Value,
        params: ReadTextFileParams,
    },
    /// Agent wants to write a file; route to the staging changeset and reply.
    FileWriteRequest {
        request_id: Value,
        params: WriteTextFileParams,
    },
    /// An agent → client request with an unrecognized method (reply method-not-found).
    UnknownRequest {
        request_id: Value,
        method: String,
    },
    /// The agent's turn ended.
    TurnEnded {
        stop_reason: StopReason,
    },
    /// A non-fatal protocol error (bad response, parse failure on a known message).
    Error {
        message: String,
    },
    /// A line that wasn't valid JSON-RPC; isolated and surfaced for logging only.
    Malformed(String),
}

/// Per-session protocol state. One per active agent panel.
#[derive(Debug, Default)]
pub struct SessionState {
    /// The agent-assigned session id, once `session/new` completes.
    pub session_id: Option<String>,
    /// Outstanding client→agent requests: id → what it was.
    pending: Vec<(u64, PendingKind)>,
}

impl SessionState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that a request with `id` was sent, expecting a response of `kind`.
    pub fn expect_response(&mut self, id: u64, kind: PendingKind) {
        self.pending.push((id, kind));
    }

    fn take_pending(&mut self, id: u64) -> Option<PendingKind> {
        if let Some(pos) = self.pending.iter().position(|(i, _)| *i == id) {
            Some(self.pending.remove(pos).1)
        } else {
            None
        }
    }

    /// Process one inbound message into zero or more events.
    pub fn process(&mut self, msg: Incoming) -> Vec<AgentEvent> {
        match msg {
            Incoming::Response(r) => self.on_response(r),
            Incoming::Notification(n) => self.on_notification(n),
            Incoming::Request(r) => self.on_request(r),
            Incoming::Malformed(s) => vec![AgentEvent::Malformed(s)],
        }
    }

    fn on_response(&mut self, r: JsonRpcResponse) -> Vec<AgentEvent> {
        let id = match r.id.as_u64() {
            Some(i) => i,
            None => return vec![],
        };
        let kind = match self.take_pending(id) {
            Some(k) => k,
            // Response to an id we don't track — ignore (could be a late cancel ack).
            None => return vec![],
        };

        if let Some(err) = r.error {
            return vec![AgentEvent::Error {
                message: format!("agent error {}: {}", err.code, err.message),
            }];
        }
        let result = r.result.unwrap_or(Value::Null);

        match kind {
            PendingKind::Initialize => match serde_json::from_value::<InitializeResult>(result) {
                Ok(res) => vec![AgentEvent::Initialized {
                    protocol_version: res.protocol_version,
                }],
                Err(e) => vec![AgentEvent::Error {
                    message: format!("bad initialize result: {e}"),
                }],
            },
            PendingKind::NewSession => match serde_json::from_value::<NewSessionResult>(result) {
                Ok(res) => {
                    self.session_id = Some(res.session_id.clone());
                    vec![AgentEvent::SessionCreated {
                        session_id: res.session_id,
                    }]
                }
                Err(e) => vec![AgentEvent::Error {
                    message: format!("bad session/new result: {e}"),
                }],
            },
            PendingKind::Prompt => match serde_json::from_value::<PromptResult>(result) {
                Ok(res) => vec![AgentEvent::TurnEnded {
                    stop_reason: res.stop_reason,
                }],
                Err(e) => vec![AgentEvent::Error {
                    message: format!("bad session/prompt result: {e}"),
                }],
            },
        }
    }

    fn on_notification(&mut self, n: JsonRpcNotification) -> Vec<AgentEvent> {
        if n.method != method::SESSION_UPDATE {
            // Unknown notification: skip (version/capability gating — never fatal).
            return vec![];
        }
        let params = n.params.unwrap_or(Value::Null);
        let note: SessionNotification = match serde_json::from_value(params) {
            Ok(v) => v,
            Err(e) => {
                return vec![AgentEvent::Error {
                    message: format!("bad session/update: {e}"),
                }]
            }
        };
        vec![match note.update {
            SessionUpdate::AgentMessageChunk { content } => AgentEvent::MessageChunk {
                text: content_text(&content),
            },
            SessionUpdate::AgentThoughtChunk { content } => AgentEvent::ThoughtChunk {
                text: content_text(&content),
            },
            SessionUpdate::ToolCall(tc) => AgentEvent::ToolCallStarted(tc),
            SessionUpdate::ToolCallUpdate(u) => AgentEvent::ToolCallUpdated(u),
            SessionUpdate::Plan { entries } => AgentEvent::Plan(entries),
        }]
    }

    fn on_request(&mut self, r: JsonRpcRequest) -> Vec<AgentEvent> {
        let params = r.params.clone().unwrap_or(Value::Null);
        match r.method.as_str() {
            method::REQUEST_PERMISSION => {
                match serde_json::from_value::<RequestPermissionParams>(params) {
                    Ok(p) => vec![AgentEvent::PermissionRequest {
                        request_id: r.id,
                        params: p,
                    }],
                    Err(e) => vec![AgentEvent::Error {
                        message: format!("bad request_permission: {e}"),
                    }],
                }
            }
            method::FS_READ_TEXT_FILE => {
                match serde_json::from_value::<ReadTextFileParams>(params) {
                    Ok(p) => vec![AgentEvent::FileReadRequest {
                        request_id: r.id,
                        params: p,
                    }],
                    Err(e) => vec![AgentEvent::Error {
                        message: format!("bad fs/read_text_file: {e}"),
                    }],
                }
            }
            method::FS_WRITE_TEXT_FILE => {
                match serde_json::from_value::<WriteTextFileParams>(params) {
                    Ok(p) => vec![AgentEvent::FileWriteRequest {
                        request_id: r.id,
                        params: p,
                    }],
                    Err(e) => vec![AgentEvent::Error {
                        message: format!("bad fs/write_text_file: {e}"),
                    }],
                }
            }
            other => vec![AgentEvent::UnknownRequest {
                request_id: r.id,
                method: other.to_string(),
            }],
        }
    }
}

/// Extract the plain text of a content block (non-text blocks render as a label).
fn content_text(block: &ContentBlock) -> String {
    match block {
        ContentBlock::Text { text } => text.clone(),
        ContentBlock::Resource { resource } => resource.text.clone(),
        ContentBlock::ResourceLink { name, .. } => format!("[{name}]"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::classify;
    use serde_json::json;

    fn incoming(v: Value) -> Incoming {
        classify(v)
    }

    #[test]
    fn initialize_response_routes_to_event() {
        let mut s = SessionState::new();
        s.expect_response(1, PendingKind::Initialize);
        let ev = s.process(incoming(json!({
            "jsonrpc":"2.0","id":1,"result":{"protocolVersion":1}
        })));
        assert_eq!(
            ev,
            vec![AgentEvent::Initialized {
                protocol_version: 1
            }]
        );
    }

    #[test]
    fn new_session_sets_id() {
        let mut s = SessionState::new();
        s.expect_response(2, PendingKind::NewSession);
        let ev = s.process(incoming(json!({
            "jsonrpc":"2.0","id":2,"result":{"sessionId":"sess-1"}
        })));
        assert_eq!(
            ev,
            vec![AgentEvent::SessionCreated {
                session_id: "sess-1".into()
            }]
        );
        assert_eq!(s.session_id.as_deref(), Some("sess-1"));
    }

    #[test]
    fn streaming_message_chunks() {
        let mut s = SessionState::new();
        let ev = s.process(incoming(json!({
            "jsonrpc":"2.0","method":"session/update","params":{
                "sessionId":"sess-1",
                "update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"Hello "}}
            }
        })));
        assert_eq!(
            ev,
            vec![AgentEvent::MessageChunk {
                text: "Hello ".into()
            }]
        );
    }

    #[test]
    fn tool_call_lifecycle() {
        let mut s = SessionState::new();
        let started = s.process(incoming(json!({
            "jsonrpc":"2.0","method":"session/update","params":{
                "sessionId":"s","update":{
                    "sessionUpdate":"tool_call","toolCallId":"t1","title":"Edit main.rs","kind":"edit","status":"pending"
                }
            }
        })));
        assert!(
            matches!(started.as_slice(), [AgentEvent::ToolCallStarted(tc)] if tc.tool_call_id == "t1")
        );

        let updated = s.process(incoming(json!({
            "jsonrpc":"2.0","method":"session/update","params":{
                "sessionId":"s","update":{
                    "sessionUpdate":"tool_call_update","toolCallId":"t1","status":"completed"
                }
            }
        })));
        assert!(matches!(updated.as_slice(),
            [AgentEvent::ToolCallUpdated(u)] if u.status == Some(ToolCallStatus::Completed)));
    }

    #[test]
    fn permission_request_surfaced_with_id() {
        let mut s = SessionState::new();
        let ev = s.process(incoming(json!({
            "jsonrpc":"2.0","id":42,"method":"session/request_permission","params":{
                "sessionId":"s",
                "toolCall":{"toolCallId":"t1","title":"Run tests","kind":"execute","status":"pending"},
                "options":[{"optionId":"allow","name":"Allow","kind":"allow_once"}]
            }
        })));
        match &ev[0] {
            AgentEvent::PermissionRequest { request_id, params } => {
                assert_eq!(request_id.as_u64(), Some(42));
                assert_eq!(params.options[0].option_id, "allow");
            }
            other => panic!("expected PermissionRequest, got {other:?}"),
        }
    }

    #[test]
    fn file_read_request_surfaced() {
        let mut s = SessionState::new();
        let ev = s.process(incoming(json!({
            "jsonrpc":"2.0","id":5,"method":"fs/read_text_file","params":{
                "sessionId":"s","path":"src/main.rs"
            }
        })));
        assert!(matches!(&ev[0],
            AgentEvent::FileReadRequest { params, .. } if params.path == "src/main.rs"));
    }

    #[test]
    fn unknown_notification_is_skipped() {
        let mut s = SessionState::new();
        let ev = s.process(incoming(json!({
            "jsonrpc":"2.0","method":"some/future/method","params":{}
        })));
        assert!(ev.is_empty());
    }

    #[test]
    fn unknown_request_reports_for_error_reply() {
        let mut s = SessionState::new();
        let ev = s.process(incoming(json!({
            "jsonrpc":"2.0","id":9,"method":"future/request","params":{}
        })));
        assert!(matches!(&ev[0],
            AgentEvent::UnknownRequest { method, .. } if method == "future/request"));
    }

    #[test]
    fn malformed_is_isolated() {
        let mut s = SessionState::new();
        let ev = s.process(Incoming::Malformed("{bad".into()));
        assert!(matches!(&ev[0], AgentEvent::Malformed(_)));
    }

    #[test]
    fn response_to_unknown_id_ignored() {
        let mut s = SessionState::new();
        let ev = s.process(incoming(json!({"jsonrpc":"2.0","id":999,"result":{}})));
        assert!(ev.is_empty());
    }

    #[test]
    fn prompt_result_ends_turn() {
        let mut s = SessionState::new();
        s.expect_response(3, PendingKind::Prompt);
        let ev = s.process(incoming(json!({
            "jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}
        })));
        assert_eq!(
            ev,
            vec![AgentEvent::TurnEnded {
                stop_reason: StopReason::EndTurn
            }]
        );
    }

    #[test]
    fn error_response_surfaces_error() {
        let mut s = SessionState::new();
        s.expect_response(1, PendingKind::Initialize);
        let ev = s.process(incoming(json!({
            "jsonrpc":"2.0","id":1,"error":{"code":-32603,"message":"boom"}
        })));
        assert!(matches!(&ev[0], AgentEvent::Error { message } if message.contains("boom")));
    }
}
