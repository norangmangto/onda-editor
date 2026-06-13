//! DAP session state machine (W15.1).
//!
//! Pure logic: feed inbound [`Incoming`] messages, get back [`DapEvent`]s the editor
//! consumes. Responses are routed by `request_seq` to the command that was sent, so
//! the full surface — handshake, stop/continue/exit, stack/scopes/variables, evaluate
//! — is unit-testable without spawning an adapter.

use crate::protocol::*;
use crate::transport::Incoming;

/// What a client-sent request was, so its response can be routed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingKind {
    Initialize,
    Launch,
    SetBreakpoints,
    ConfigurationDone,
    Threads,
    StackTrace,
    Scopes,
    Variables,
    Continue,
    Step,
    Evaluate,
    Disconnect,
}

/// Events surfaced to the editor from the debug adapter.
#[derive(Debug, Clone, PartialEq)]
pub enum DapEvent {
    /// `initialize` response received (adapter capabilities ready) → send launch.
    AdapterReady,
    /// `initialized` event received → send breakpoints + configurationDone.
    ConfigReady,
    /// Breakpoints were (re)set; carries the adapter's verification result.
    BreakpointsSet(Vec<Breakpoint>),
    /// Execution stopped (breakpoint / step / exception).
    Stopped {
        thread_id: Option<i64>,
        reason: String,
    },
    Continued,
    Exited {
        code: i64,
    },
    Terminated,
    Output {
        category: String,
        text: String,
    },
    Threads(Vec<Thread>),
    StackTrace(Vec<StackFrame>),
    Scopes(Vec<Scope>),
    Variables(Vec<Variable>),
    Evaluated {
        result: String,
    },
    /// A non-fatal error (failed response, parse error).
    Error(String),
    /// An unparseable frame; surfaced for logging only.
    Malformed(String),
}

/// Per-session protocol state.
#[derive(Debug, Default)]
pub struct DapSession {
    pending: Vec<(i64, PendingKind)>,
    /// Thread id of the most recent stop (the focused thread).
    pub stopped_thread: Option<i64>,
}

impl DapSession {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn expect(&mut self, seq: i64, kind: PendingKind) {
        self.pending.push((seq, kind));
    }

    fn take(&mut self, seq: i64) -> Option<PendingKind> {
        self.pending
            .iter()
            .position(|(s, _)| *s == seq)
            .map(|i| self.pending.remove(i).1)
    }

    pub fn process(&mut self, msg: Incoming) -> Vec<DapEvent> {
        match msg {
            Incoming::Response(r) => self.on_response(r),
            Incoming::Event(e) => self.on_event(e),
            Incoming::Malformed(s) => vec![DapEvent::Malformed(s)],
        }
    }

    fn on_response(&mut self, r: Response) -> Vec<DapEvent> {
        let kind = match self.take(r.request_seq) {
            Some(k) => k,
            None => return vec![],
        };
        if !r.success {
            let msg = r.message.unwrap_or_else(|| r.command.clone());
            return vec![DapEvent::Error(format!("{}: {msg}", r.command))];
        }
        let body = r.body.unwrap_or(serde_json::Value::Null);
        match kind {
            PendingKind::Initialize => vec![DapEvent::AdapterReady],
            PendingKind::Launch | PendingKind::ConfigurationDone | PendingKind::Continue => {
                // Launch/config acks carry no UI payload; continue clears the stop.
                if kind == PendingKind::Continue {
                    self.stopped_thread = None;
                    vec![DapEvent::Continued]
                } else {
                    vec![]
                }
            }
            PendingKind::Step => vec![], // the resulting `stopped` event drives the UI
            PendingKind::SetBreakpoints => match serde_json::from_value::<SetBreakpointsBody>(body)
            {
                Ok(b) => vec![DapEvent::BreakpointsSet(b.breakpoints)],
                Err(e) => vec![DapEvent::Error(format!("bad setBreakpoints body: {e}"))],
            },
            PendingKind::Threads => match serde_json::from_value::<ThreadsBody>(body) {
                Ok(b) => vec![DapEvent::Threads(b.threads)],
                Err(e) => vec![DapEvent::Error(format!("bad threads body: {e}"))],
            },
            PendingKind::StackTrace => match serde_json::from_value::<StackTraceBody>(body) {
                Ok(b) => vec![DapEvent::StackTrace(b.stack_frames)],
                Err(e) => vec![DapEvent::Error(format!("bad stackTrace body: {e}"))],
            },
            PendingKind::Scopes => match serde_json::from_value::<ScopesBody>(body) {
                Ok(b) => vec![DapEvent::Scopes(b.scopes)],
                Err(e) => vec![DapEvent::Error(format!("bad scopes body: {e}"))],
            },
            PendingKind::Variables => match serde_json::from_value::<VariablesBody>(body) {
                Ok(b) => vec![DapEvent::Variables(b.variables)],
                Err(e) => vec![DapEvent::Error(format!("bad variables body: {e}"))],
            },
            PendingKind::Evaluate => match serde_json::from_value::<EvaluateBody>(body) {
                Ok(b) => vec![DapEvent::Evaluated { result: b.result }],
                Err(e) => vec![DapEvent::Error(format!("bad evaluate body: {e}"))],
            },
            PendingKind::Disconnect => vec![DapEvent::Terminated],
        }
    }

    fn on_event(&mut self, e: Event) -> Vec<DapEvent> {
        let body = e.body.unwrap_or(serde_json::Value::Null);
        match e.event.as_str() {
            event::INITIALIZED => vec![DapEvent::ConfigReady],
            event::STOPPED => match serde_json::from_value::<StoppedBody>(body) {
                Ok(b) => {
                    self.stopped_thread = b.thread_id;
                    vec![DapEvent::Stopped {
                        thread_id: b.thread_id,
                        reason: b.reason,
                    }]
                }
                Err(e) => vec![DapEvent::Error(format!("bad stopped body: {e}"))],
            },
            event::CONTINUED => {
                self.stopped_thread = None;
                vec![DapEvent::Continued]
            }
            event::EXITED => {
                let code = serde_json::from_value::<ExitedBody>(body)
                    .map(|b| b.exit_code)
                    .unwrap_or(0);
                vec![DapEvent::Exited { code }]
            }
            event::TERMINATED => vec![DapEvent::Terminated],
            event::OUTPUT => match serde_json::from_value::<OutputBody>(body) {
                Ok(b) => vec![DapEvent::Output {
                    category: b.category.unwrap_or_else(|| "console".into()),
                    text: b.output,
                }],
                Err(_) => vec![],
            },
            // thread / breakpoint events: not surfaced to the UI in v1.
            _ => vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::classify;
    use serde_json::json;

    fn inc(v: serde_json::Value) -> Incoming {
        classify(v)
    }

    #[test]
    fn initialize_response_to_adapter_ready() {
        let mut s = DapSession::new();
        s.expect(1, PendingKind::Initialize);
        let ev = s.process(inc(json!({
            "seq":2,"type":"response","request_seq":1,"success":true,"command":"initialize",
            "body":{"supportsConfigurationDoneRequest":true}
        })));
        assert_eq!(ev, vec![DapEvent::AdapterReady]);
    }

    #[test]
    fn initialized_event_to_config_ready() {
        let mut s = DapSession::new();
        let ev = s.process(inc(json!({"seq":3,"type":"event","event":"initialized"})));
        assert_eq!(ev, vec![DapEvent::ConfigReady]);
    }

    #[test]
    fn stopped_records_thread() {
        let mut s = DapSession::new();
        let ev = s.process(inc(json!({
            "seq":4,"type":"event","event":"stopped",
            "body":{"reason":"breakpoint","threadId":1}
        })));
        assert_eq!(
            ev,
            vec![DapEvent::Stopped {
                thread_id: Some(1),
                reason: "breakpoint".into()
            }]
        );
        assert_eq!(s.stopped_thread, Some(1));
    }

    #[test]
    fn stack_trace_response_parsed() {
        let mut s = DapSession::new();
        s.expect(5, PendingKind::StackTrace);
        let ev = s.process(inc(json!({
            "seq":6,"type":"response","request_seq":5,"success":true,"command":"stackTrace",
            "body":{"stackFrames":[{"id":1,"name":"main","line":10,"column":1}],"totalFrames":1}
        })));
        match &ev[0] {
            DapEvent::StackTrace(frames) => {
                assert_eq!(frames[0].name, "main");
                assert_eq!(frames[0].line, 10);
            }
            other => panic!("expected StackTrace, got {other:?}"),
        }
    }

    #[test]
    fn variables_response_parsed() {
        let mut s = DapSession::new();
        s.expect(7, PendingKind::Variables);
        let ev = s.process(inc(json!({
            "seq":8,"type":"response","request_seq":7,"success":true,"command":"variables",
            "body":{"variables":[{"name":"x","value":"42","type":"i32","variablesReference":0}]}
        })));
        match &ev[0] {
            DapEvent::Variables(vars) => {
                assert_eq!(vars[0].name, "x");
                assert_eq!(vars[0].value, "42");
                assert_eq!(vars[0].ty.as_deref(), Some("i32"));
            }
            other => panic!("expected Variables, got {other:?}"),
        }
    }

    #[test]
    fn evaluate_response_parsed() {
        let mut s = DapSession::new();
        s.expect(9, PendingKind::Evaluate);
        let ev = s.process(inc(json!({
            "seq":10,"type":"response","request_seq":9,"success":true,"command":"evaluate",
            "body":{"result":"3","variablesReference":0}
        })));
        assert_eq!(ev, vec![DapEvent::Evaluated { result: "3".into() }]);
    }

    #[test]
    fn failed_response_is_error() {
        let mut s = DapSession::new();
        s.expect(1, PendingKind::Launch);
        let ev = s.process(inc(json!({
            "seq":2,"type":"response","request_seq":1,"success":false,"command":"launch",
            "message":"no such program"
        })));
        assert!(matches!(&ev[0], DapEvent::Error(m) if m.contains("no such program")));
    }

    #[test]
    fn exited_and_terminated() {
        let mut s = DapSession::new();
        let ex = s.process(inc(json!({
            "seq":1,"type":"event","event":"exited","body":{"exitCode":0}
        })));
        assert_eq!(ex, vec![DapEvent::Exited { code: 0 }]);
        let te = s.process(inc(json!({"seq":2,"type":"event","event":"terminated"})));
        assert_eq!(te, vec![DapEvent::Terminated]);
    }

    #[test]
    fn unknown_response_seq_ignored() {
        let mut s = DapSession::new();
        let ev = s.process(inc(json!({
            "seq":1,"type":"response","request_seq":999,"success":true,"command":"threads"
        })));
        assert!(ev.is_empty());
    }

    #[test]
    fn malformed_isolated() {
        let mut s = DapSession::new();
        assert!(matches!(
            s.process(Incoming::Malformed("x".into()))[0],
            DapEvent::Malformed(_)
        ));
    }
}
