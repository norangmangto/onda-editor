//! Vendored Agent Client Protocol (ACP) types (onda T22.0/T22.2).
//!
//! ACP is JSON-RPC 2.0 over stdio with newline-delimited messages. The upstream
//! spec is still moving, so onda vendors a faithful subset here behind a thin
//! adapter rather than depending on the churning `agent-client-protocol` crate
//! (see AGENTS.md). Field names use the wire `camelCase` form via serde renames.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Protocol version onda implements. Sent in `initialize`.
pub const PROTOCOL_VERSION: u32 = 1;

// ── Method names ────────────────────────────────────────────────────────────────

pub mod method {
    // Client → agent.
    pub const INITIALIZE: &str = "initialize";
    pub const SESSION_NEW: &str = "session/new";
    pub const SESSION_PROMPT: &str = "session/prompt";
    pub const SESSION_CANCEL: &str = "session/cancel";
    // Agent → client (notifications / requests).
    pub const SESSION_UPDATE: &str = "session/update";
    pub const REQUEST_PERMISSION: &str = "session/request_permission";
    pub const FS_READ_TEXT_FILE: &str = "fs/read_text_file";
    pub const FS_WRITE_TEXT_FILE: &str = "fs/write_text_file";
}

// ── Content blocks ──────────────────────────────────────────────────────────────

/// A piece of prompt/response content. Mentions (`@file` etc.) are sent as
/// `resource` blocks; plain text as `text`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    /// An embedded resource (e.g. an attached `@file`/`@selection`).
    Resource {
        resource: EmbeddedResource,
    },
    /// A link to a resource the agent may fetch on demand.
    ResourceLink {
        uri: String,
        name: String,
    },
}

impl ContentBlock {
    pub fn text(s: impl Into<String>) -> Self {
        ContentBlock::Text { text: s.into() }
    }
}

/// Inlined resource contents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddedResource {
    pub uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    pub text: String,
}

// ── initialize ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    pub protocol_version: u32,
    #[serde(default)]
    pub client_capabilities: ClientCapabilities,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientCapabilities {
    /// Whether the client serves `fs/read_text_file` / `fs/write_text_file`.
    #[serde(default)]
    pub fs: FsCapabilities,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FsCapabilities {
    pub read_text_file: bool,
    pub write_text_file: bool,
}

impl Default for FsCapabilities {
    fn default() -> Self {
        Self {
            read_text_file: true,
            write_text_file: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    pub protocol_version: u32,
    #[serde(default)]
    pub agent_capabilities: Value,
}

// ── session/new ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewSessionParams {
    pub cwd: String,
    #[serde(default)]
    pub mcp_servers: Vec<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewSessionResult {
    pub session_id: String,
}

// ── session/prompt ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptParams {
    pub session_id: String,
    pub prompt: Vec<ContentBlock>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptResult {
    pub stop_reason: StopReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    Cancelled,
    Refusal,
}

// ── session/update (streaming notification payload) ─────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionNotification {
    pub session_id: String,
    pub update: SessionUpdate,
}

/// Streamed updates delivered via the `session/update` notification.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "sessionUpdate", rename_all = "snake_case")]
pub enum SessionUpdate {
    AgentMessageChunk { content: ContentBlock },
    AgentThoughtChunk { content: ContentBlock },
    ToolCall(ToolCall),
    ToolCallUpdate(ToolCallUpdate),
    Plan { entries: Vec<PlanEntry> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCall {
    pub tool_call_id: String,
    pub title: String,
    #[serde(default)]
    pub kind: ToolKind,
    #[serde(default)]
    pub status: ToolCallStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_input: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallUpdate {
    pub tool_call_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<ToolCallStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    #[default]
    Other,
    Read,
    Edit,
    Delete,
    Execute,
    Fetch,
    Search,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallStatus {
    #[default]
    Pending,
    InProgress,
    Completed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanEntry {
    pub content: String,
    #[serde(default)]
    pub status: PlanEntryStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanEntryStatus {
    #[default]
    Pending,
    InProgress,
    Completed,
}

// ── session/request_permission (agent → client) ─────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RequestPermissionParams {
    pub session_id: String,
    pub tool_call: ToolCall,
    pub options: Vec<PermissionOption>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionOption {
    pub option_id: String,
    pub name: String,
    pub kind: PermissionOptionKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionOptionKind {
    AllowOnce,
    AllowAlways,
    RejectOnce,
    RejectAlways,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestPermissionResult {
    pub outcome: PermissionOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "camelCase")]
pub enum PermissionOutcome {
    Selected {
        #[serde(rename = "optionId")]
        option_id: String,
    },
    Cancelled,
}

// ── fs/read_text_file & fs/write_text_file (agent → client) ─────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReadTextFileParams {
    pub session_id: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadTextFileResult {
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WriteTextFileParams {
    pub session_id: String,
    pub path: String,
    pub content: String,
}

// ── session/cancel ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelParams {
    pub session_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip<T: Serialize + serde::de::DeserializeOwned + std::fmt::Debug>(v: &T) -> Value {
        let j = serde_json::to_value(v).unwrap();
        // Ensure it deserializes back.
        let _back: T = serde_json::from_value(j.clone()).unwrap();
        j
    }

    #[test]
    fn content_block_text_wire_shape() {
        let j = roundtrip(&ContentBlock::text("hello"));
        assert_eq!(j["type"], "text");
        assert_eq!(j["text"], "hello");
    }

    #[test]
    fn session_update_tagged_by_session_update_field() {
        let upd = SessionUpdate::AgentMessageChunk {
            content: ContentBlock::text("hi"),
        };
        let j = roundtrip(&upd);
        assert_eq!(j["sessionUpdate"], "agent_message_chunk");
        assert_eq!(j["content"]["text"], "hi");
    }

    #[test]
    fn tool_call_camel_case() {
        let tc = ToolCall {
            tool_call_id: "t1".into(),
            title: "Write file".into(),
            kind: ToolKind::Edit,
            status: ToolCallStatus::Pending,
            raw_input: None,
        };
        let j = roundtrip(&tc);
        assert_eq!(j["toolCallId"], "t1");
        assert_eq!(j["kind"], "edit");
        assert_eq!(j["status"], "pending");
    }

    #[test]
    fn permission_outcome_internally_tagged() {
        let out = PermissionOutcome::Selected {
            option_id: "allow".into(),
        };
        let j = roundtrip(&out);
        assert_eq!(j["outcome"], "selected");
        assert_eq!(j["optionId"], "allow");
    }

    #[test]
    fn prompt_params_roundtrip() {
        let p = PromptParams {
            session_id: "s1".into(),
            prompt: vec![ContentBlock::text("do it")],
        };
        let j = roundtrip(&p);
        assert_eq!(j["sessionId"], "s1");
        assert_eq!(j["prompt"][0]["text"], "do it");
    }

    #[test]
    fn read_file_params_optional_fields_omitted() {
        let p = ReadTextFileParams {
            session_id: "s1".into(),
            path: "src/main.rs".into(),
            line: None,
            limit: None,
        };
        let j = roundtrip(&p);
        assert!(j.get("line").is_none());
        assert_eq!(j["path"], "src/main.rs");
    }
}
