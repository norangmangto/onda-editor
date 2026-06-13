//! Scriptable mock ACP agent for tests & CI (onda T22.0).
//!
//! Speaks NDJSON JSON-RPC on stdio. The scenario is chosen via the
//! `ONDA_MOCK_SCENARIO` env var; each drives a different protocol path so the
//! client's conformance can be exercised headlessly:
//!
//! - `stream`     (default) — two message chunks + a plan, then `end_turn`
//! - `tool`       — a tool_call + tool_call_update, then `end_turn`
//! - `permission` — request_permission, wait for the reply, then `end_turn`
//! - `fileread`   — fs/read_text_file, echo the returned content, then `end_turn`
//! - `malformed`  — emit a garbage line before finishing (client must isolate it)
//! - `die`        — exit mid-stream (client must show disconnected)

use std::io::{self, BufRead, Write};

use serde_json::{json, Value};

fn main() {
    let scenario = std::env::var("ONDA_MOCK_SCENARIO").unwrap_or_else(|_| "stream".into());
    let stdin = io::stdin();
    let mut reader = stdin.lock();
    let mut line = String::new();

    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break, // EOF
            Ok(_) => {}
            Err(_) => break,
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let v: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let id = v.get("id").cloned();
        let method = v.get("method").and_then(|m| m.as_str()).unwrap_or("");

        match method {
            "initialize" => respond(&id, json!({ "protocolVersion": 1 })),
            "session/new" => respond(&id, json!({ "sessionId": "mock-session" })),
            "session/prompt" => handle_prompt(&scenario, &id, &mut reader),
            "session/cancel" => { /* notification: nothing to ack */ }
            _ => {
                if id.is_some() {
                    respond_err(&id, -32601, "method not found");
                }
            }
        }
    }
}

fn handle_prompt(scenario: &str, id: &Option<Value>, reader: &mut impl BufRead) {
    chunk("Hello ");
    chunk("world");

    match scenario {
        "tool" => {
            notify(
                "session/update",
                json!({
                    "sessionId": "mock-session",
                    "update": {
                        "sessionUpdate": "tool_call",
                        "toolCallId": "t1",
                        "title": "Edit src/main.rs",
                        "kind": "edit",
                        "status": "pending"
                    }
                }),
            );
            notify(
                "session/update",
                json!({
                    "sessionId": "mock-session",
                    "update": {
                        "sessionUpdate": "tool_call_update",
                        "toolCallId": "t1",
                        "status": "completed"
                    }
                }),
            );
        }
        "permission" => {
            // Ask for permission, then block for the client's reply.
            send_request(
                "p1",
                "session/request_permission",
                json!({
                    "sessionId": "mock-session",
                    "toolCall": {
                        "toolCallId": "t1", "title": "Run tests",
                        "kind": "execute", "status": "pending"
                    },
                    "options": [
                        { "optionId": "allow", "name": "Allow", "kind": "allow_once" },
                        { "optionId": "deny", "name": "Deny", "kind": "reject_once" }
                    ]
                }),
            );
            let reply = read_one(reader);
            // Echo the chosen option back as a message chunk so tests can observe it.
            if let Some(opt) = reply
                .get("result")
                .and_then(|r| r.get("outcome"))
                .and_then(|o| o.get("optionId"))
                .and_then(|s| s.as_str())
            {
                chunk(&format!("permission:{opt}"));
            }
        }
        "fileread" => {
            send_request(
                "f1",
                "fs/read_text_file",
                json!({ "sessionId": "mock-session", "path": "src/main.rs" }),
            );
            let reply = read_one(reader);
            if let Some(content) = reply
                .get("result")
                .and_then(|r| r.get("content"))
                .and_then(|s| s.as_str())
            {
                chunk(&format!("file:{content}"));
            }
        }
        "malformed" => {
            // A line that is not valid JSON — the client must isolate it.
            println!("this is not json");
            let _ = io::stdout().flush();
        }
        "die" => {
            std::process::exit(1);
        }
        _ => {
            // Default "stream": include a small plan.
            notify(
                "session/update",
                json!({
                    "sessionId": "mock-session",
                    "update": {
                        "sessionUpdate": "plan",
                        "entries": [ { "content": "do the thing", "status": "pending" } ]
                    }
                }),
            );
        }
    }

    respond(id, json!({ "stopReason": "end_turn" }));
}

fn chunk(text: &str) {
    notify(
        "session/update",
        json!({
            "sessionId": "mock-session",
            "update": {
                "sessionUpdate": "agent_message_chunk",
                "content": { "type": "text", "text": text }
            }
        }),
    );
}

fn write_line(v: &Value) {
    let mut out = io::stdout();
    let _ = writeln!(out, "{v}");
    let _ = out.flush();
}

fn respond(id: &Option<Value>, result: Value) {
    if let Some(id) = id {
        write_line(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));
    }
}

fn respond_err(id: &Option<Value>, code: i64, message: &str) {
    if let Some(id) = id {
        write_line(&json!({
            "jsonrpc": "2.0", "id": id,
            "error": { "code": code, "message": message }
        }));
    }
}

fn notify(method: &str, params: Value) {
    write_line(&json!({ "jsonrpc": "2.0", "method": method, "params": params }));
}

fn send_request(id: &str, method: &str, params: Value) {
    write_line(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
}

fn read_one(reader: &mut impl BufRead) -> Value {
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => return Value::Null,
            Ok(_) => {
                let t = line.trim();
                if t.is_empty() {
                    continue;
                }
                return serde_json::from_str(t).unwrap_or(Value::Null);
            }
            Err(_) => return Value::Null,
        }
    }
}
