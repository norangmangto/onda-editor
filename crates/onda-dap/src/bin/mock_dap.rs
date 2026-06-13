//! Scriptable mock DAP adapter for tests & CI (onda W15.1).
//!
//! Speaks Content-Length framed DAP on stdio and drives a full session: hit a
//! breakpoint after `configurationDone`, answer threads/stackTrace/scopes/variables/
//! evaluate, and terminate on `continue`. The real targets (`lldb-dap`, `debugpy`)
//! are documented in `docs/DAP.md`; this mock owns protocol conformance in CI.

use std::io::{self, BufRead, Write};

use serde_json::{json, Value};

fn main() {
    let stdin = io::stdin();
    let mut reader = stdin.lock();
    let mut seq = 1000i64;
    // The breakpoint line (echoed back in stackTrace), captured from setBreakpoints.
    let mut bp_line = 1u32;
    let mut bp_path = String::from("unknown");

    while let Some(msg) = read_frame(&mut reader) {
        let command = msg.get("command").and_then(|c| c.as_str()).unwrap_or("");
        let req_seq = msg.get("seq").and_then(|s| s.as_i64()).unwrap_or(0);
        let args = msg.get("arguments").cloned().unwrap_or(Value::Null);

        match command {
            "initialize" => {
                respond(
                    &mut seq,
                    req_seq,
                    command,
                    json!({"supportsConfigurationDoneRequest": true}),
                );
                emit(&mut seq, "initialized", Value::Null);
            }
            "launch" | "attach" => respond(&mut seq, req_seq, command, Value::Null),
            "setBreakpoints" => {
                let bps = args
                    .get("breakpoints")
                    .and_then(|b| b.as_array())
                    .cloned()
                    .unwrap_or_default();
                if let Some(p) = args
                    .get("source")
                    .and_then(|s| s.get("path"))
                    .and_then(|p| p.as_str())
                {
                    bp_path = p.to_string();
                }
                if let Some(first) = bps
                    .first()
                    .and_then(|b| b.get("line"))
                    .and_then(|l| l.as_u64())
                {
                    bp_line = first as u32;
                }
                let verified: Vec<Value> = bps
                    .iter()
                    .map(|b| json!({"verified": true, "line": b.get("line")}))
                    .collect();
                respond(&mut seq, req_seq, command, json!({"breakpoints": verified}));
            }
            "configurationDone" => {
                respond(&mut seq, req_seq, command, Value::Null);
                // Program "runs" and immediately hits the breakpoint.
                emit(
                    &mut seq,
                    "stopped",
                    json!({"reason":"breakpoint","threadId":1}),
                );
            }
            "threads" => respond(
                &mut seq,
                req_seq,
                command,
                json!({"threads":[{"id":1,"name":"main"}]}),
            ),
            "stackTrace" => respond(
                &mut seq,
                req_seq,
                command,
                json!({
                    "stackFrames":[
                        {"id":1,"name":"main","line":bp_line,"column":1,
                         "source":{"name":"main","path":bp_path}}
                    ],
                    "totalFrames":1
                }),
            ),
            "scopes" => respond(
                &mut seq,
                req_seq,
                command,
                json!({"scopes":[{"name":"Locals","variablesReference":1000,"expensive":false}]}),
            ),
            "variables" => respond(
                &mut seq,
                req_seq,
                command,
                json!({
                    "variables":[
                        {"name":"x","value":"42","type":"i32","variablesReference":0},
                        {"name":"vec","value":"Vec(len: 3)","type":"Vec<i32>","variablesReference":1001}
                    ]
                }),
            ),
            "evaluate" => {
                let expr = args
                    .get("expression")
                    .and_then(|e| e.as_str())
                    .unwrap_or("");
                respond(
                    &mut seq,
                    req_seq,
                    command,
                    json!({"result": format!("{expr} = 3"), "variablesReference": 0}),
                );
            }
            "next" | "stepIn" | "stepOut" => {
                respond(&mut seq, req_seq, command, Value::Null);
                emit(&mut seq, "stopped", json!({"reason":"step","threadId":1}));
            }
            "continue" => {
                respond(
                    &mut seq,
                    req_seq,
                    command,
                    json!({"allThreadsContinued": true}),
                );
                emit(&mut seq, "exited", json!({"exitCode":0}));
                emit(&mut seq, "terminated", Value::Null);
            }
            "disconnect" | "terminate" => {
                respond(&mut seq, req_seq, command, Value::Null);
                return;
            }
            _ => respond(&mut seq, req_seq, command, Value::Null),
        }
    }
}

fn respond(seq: &mut i64, request_seq: i64, command: &str, body: Value) {
    let s = next(seq);
    let mut msg = json!({
        "seq": s, "type": "response", "request_seq": request_seq,
        "success": true, "command": command
    });
    if !body.is_null() {
        msg["body"] = body;
    }
    write_frame(&msg);
}

fn emit(seq: &mut i64, event: &str, body: Value) {
    let s = next(seq);
    let mut msg = json!({ "seq": s, "type": "event", "event": event });
    if !body.is_null() {
        msg["body"] = body;
    }
    write_frame(&msg);
}

fn next(seq: &mut i64) -> i64 {
    *seq += 1;
    *seq
}

fn write_frame(value: &Value) {
    let body = serde_json::to_vec(value).unwrap();
    let mut out = io::stdout();
    let _ = write!(out, "Content-Length: {}\r\n\r\n", body.len());
    let _ = out.write_all(&body);
    let _ = out.flush();
}

fn read_frame(reader: &mut impl BufRead) -> Option<Value> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => return None,
            Ok(_) => {
                let line = line.trim();
                if line.is_empty() {
                    break;
                }
                if let Some(rest) = line.strip_prefix("Content-Length:") {
                    content_length = rest.trim().parse::<usize>().ok();
                }
            }
            Err(_) => return None,
        }
    }
    let len = content_length?;
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf).ok()?;
    serde_json::from_slice(&buf).ok()
}
