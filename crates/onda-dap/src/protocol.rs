//! Vendored Debug Adapter Protocol (DAP) types (onda W15.1).
//!
//! DAP frames each message as `Content-Length`-prefixed JSON (same framing as LSP),
//! but the payload is DAP's own `seq`/`type` envelope — *not* JSON-RPC. We vendor a
//! faithful subset behind a thin adapter so the editor UI never depends on protocol
//! churn; the `onda-mock-dap` adapter owns conformance.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Request command names (client → adapter).
pub mod command {
    pub const INITIALIZE: &str = "initialize";
    pub const LAUNCH: &str = "launch";
    pub const ATTACH: &str = "attach";
    pub const SET_BREAKPOINTS: &str = "setBreakpoints";
    pub const CONFIGURATION_DONE: &str = "configurationDone";
    pub const THREADS: &str = "threads";
    pub const STACK_TRACE: &str = "stackTrace";
    pub const SCOPES: &str = "scopes";
    pub const VARIABLES: &str = "variables";
    pub const CONTINUE: &str = "continue";
    pub const NEXT: &str = "next";
    pub const STEP_IN: &str = "stepIn";
    pub const STEP_OUT: &str = "stepOut";
    pub const EVALUATE: &str = "evaluate";
    pub const DISCONNECT: &str = "disconnect";
    pub const TERMINATE: &str = "terminate";
}

/// Event names (adapter → client).
pub mod event {
    pub const INITIALIZED: &str = "initialized";
    pub const STOPPED: &str = "stopped";
    pub const CONTINUED: &str = "continued";
    pub const EXITED: &str = "exited";
    pub const TERMINATED: &str = "terminated";
    pub const OUTPUT: &str = "output";
    pub const THREAD: &str = "thread";
    pub const BREAKPOINT: &str = "breakpoint";
}

// ── Wire envelope ────────────────────────────────────────────────────────────────

/// A request message (client → adapter).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub seq: i64,
    #[serde(rename = "type")]
    pub kind: String, // "request"
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Value>,
}

/// A response message (adapter → client).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub seq: i64,
    #[serde(rename = "type")]
    pub kind: String, // "response"
    pub request_seq: i64,
    pub success: bool,
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Value>,
}

/// An event message (adapter → client).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub seq: i64,
    #[serde(rename = "type")]
    pub kind: String, // "event"
    pub event: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Value>,
}

// ── Request arguments ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeArgs {
    pub client_id: String,
    pub adapter_id: String,
    #[serde(default)]
    pub lines_start_at1: bool,
    #[serde(default)]
    pub columns_start_at1: bool,
    pub path_format: String,
}

impl Default for InitializeArgs {
    fn default() -> Self {
        Self {
            client_id: "onda".into(),
            adapter_id: "onda".into(),
            lines_start_at1: true,
            columns_start_at1: true,
            path_format: "path".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Source {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceBreakpoint {
    pub line: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetBreakpointsArgs {
    pub source: Source,
    pub breakpoints: Vec<SourceBreakpoint>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StackTraceArgs {
    pub thread_id: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopesArgs {
    pub frame_id: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VariablesArgs {
    pub variables_reference: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadArgs {
    pub thread_id: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvaluateArgs {
    pub expression: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
}

// ── Response / event bodies ──────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoppedBody {
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExitedBody {
    pub exit_code: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputBody {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    pub output: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Thread {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadsBody {
    pub threads: Vec<Thread>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StackFrame {
    pub id: i64,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SourcePath>,
    pub line: u32,
    #[serde(default)]
    pub column: u32,
}

/// Minimal source reference inside a stack frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourcePath {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StackTraceBody {
    pub stack_frames: Vec<StackFrame>,
    #[serde(default)]
    pub total_frames: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Scope {
    pub name: String,
    pub variables_reference: i64,
    #[serde(default)]
    pub expensive: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopesBody {
    pub scopes: Vec<Scope>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Variable {
    pub name: String,
    pub value: String,
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub ty: Option<String>,
    #[serde(default)]
    pub variables_reference: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariablesBody {
    pub variables: Vec<Variable>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvaluateBody {
    pub result: String,
    #[serde(default)]
    pub variables_reference: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Breakpoint {
    #[serde(default)]
    pub verified: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetBreakpointsBody {
    pub breakpoints: Vec<Breakpoint>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn request_wire_shape() {
        let r = Request {
            seq: 1,
            kind: "request".into(),
            command: "initialize".into(),
            arguments: Some(json!({"clientID":"onda"})),
        };
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["type"], "request");
        assert_eq!(v["command"], "initialize");
        assert_eq!(v["seq"], 1);
    }

    #[test]
    fn stopped_body_camel_case() {
        let b: StoppedBody =
            serde_json::from_value(json!({"reason":"breakpoint","threadId":1})).unwrap();
        assert_eq!(b.reason, "breakpoint");
        assert_eq!(b.thread_id, Some(1));
    }

    #[test]
    fn stack_frame_roundtrip() {
        let f = StackFrame {
            id: 3,
            name: "main".into(),
            source: Some(SourcePath {
                name: Some("main.rs".into()),
                path: Some("/p/main.rs".into()),
            }),
            line: 10,
            column: 1,
        };
        let v = serde_json::to_value(&f).unwrap();
        let back: StackFrame = serde_json::from_value(v).unwrap();
        assert_eq!(back, f);
    }

    #[test]
    fn variable_type_field_renamed() {
        let v: Variable = serde_json::from_value(json!({
            "name":"x","value":"42","type":"i32","variablesReference":0
        }))
        .unwrap();
        assert_eq!(v.ty.as_deref(), Some("i32"));
        assert_eq!(v.value, "42");
    }
}
