//! End-to-end DAP conformance test driving the real `onda-mock-dap` subprocess
//! through a full session: launch → breakpoint → stack/scopes/variables/evaluate →
//! continue → exit. Mirrors the W15 acceptance flow without a real adapter.

use std::time::Duration;

use onda_dap::{AdapterConfig, DapClient, DapEvent, SourceBreakpoint};
use serde_json::json;
use tokio::sync::mpsc;

const MOCK: &str = env!("CARGO_BIN_EXE_onda-mock-dap");

fn config() -> AdapterConfig {
    AdapterConfig {
        name: "mock".into(),
        command: MOCK.into(),
        args: vec![],
        env: vec![],
        languages: vec!["rust".into()],
        launch: json!({ "program": "/tmp/a.out" }),
    }
}

async fn next(rx: &mut mpsc::Receiver<DapEvent>) -> DapEvent {
    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for dap event")
        .expect("dap event channel closed")
}

#[tokio::test]
async fn full_debug_session() {
    let (tx, mut rx) = mpsc::channel(64);
    let cwd = std::env::current_dir().unwrap();
    let client = DapClient::launch(&config(), cwd, tx).await.unwrap();

    // Set a breakpoint at line 10 before/around the handshake.
    client
        .set_breakpoints(
            std::path::PathBuf::from("/tmp/main.rs"),
            vec![SourceBreakpoint {
                line: 10,
                condition: None,
            }],
        )
        .await
        .unwrap();

    // Drive: wait for the breakpoint stop, verifying the handshake completed.
    let mut verified = false;
    let stopped_thread;
    loop {
        match next(&mut rx).await {
            DapEvent::BreakpointsSet(bps) => {
                assert!(bps.iter().all(|b| b.verified));
                verified = true;
            }
            DapEvent::Stopped { thread_id, reason } => {
                assert_eq!(reason, "breakpoint");
                stopped_thread = thread_id.unwrap_or(1);
                break;
            }
            DapEvent::Error(e) => panic!("dap error: {e}"),
            _ => {}
        }
    }
    assert!(verified, "breakpoints should be verified");

    // Inspect the call stack.
    client.stack_trace(stopped_thread).await.unwrap();
    let frame_id;
    loop {
        if let DapEvent::StackTrace(frames) = next(&mut rx).await {
            assert_eq!(frames[0].name, "main");
            assert_eq!(frames[0].line, 10, "stack frame at the breakpoint line");
            frame_id = frames[0].id;
            break;
        }
    }

    // Scopes → variables.
    client.scopes(frame_id).await.unwrap();
    let var_ref;
    loop {
        if let DapEvent::Scopes(scopes) = next(&mut rx).await {
            assert_eq!(scopes[0].name, "Locals");
            var_ref = scopes[0].variables_reference;
            break;
        }
    }
    client.variables(var_ref).await.unwrap();
    loop {
        if let DapEvent::Variables(vars) = next(&mut rx).await {
            let x = vars.iter().find(|v| v.name == "x").unwrap();
            assert_eq!(x.value, "42");
            assert_eq!(x.ty.as_deref(), Some("i32"));
            break;
        }
    }

    // Evaluate an expression in the stopped frame.
    client
        .evaluate("vec.len()".into(), Some(frame_id))
        .await
        .unwrap();
    loop {
        if let DapEvent::Evaluated { result } = next(&mut rx).await {
            assert!(result.contains("vec.len()"));
            break;
        }
    }

    // Continue → program exits and terminates.
    client.continue_(stopped_thread).await.unwrap();
    let mut exited = false;
    let mut terminated = false;
    while !(exited && terminated) {
        match next(&mut rx).await {
            DapEvent::Exited { code } => {
                assert_eq!(code, 0);
                exited = true;
            }
            DapEvent::Terminated => terminated = true,
            _ => {}
        }
    }
}

#[tokio::test]
async fn step_produces_new_stop() {
    let (tx, mut rx) = mpsc::channel(64);
    let cwd = std::env::current_dir().unwrap();
    let client = DapClient::launch(&config(), cwd, tx).await.unwrap();

    // Wait for the initial breakpoint stop.
    let thread;
    loop {
        if let DapEvent::Stopped { thread_id, .. } = next(&mut rx).await {
            thread = thread_id.unwrap_or(1);
            break;
        }
    }
    // Step over → a new stop with reason "step".
    client.next(thread).await.unwrap();
    loop {
        if let DapEvent::Stopped { reason, .. } = next(&mut rx).await {
            assert_eq!(reason, "step");
            break;
        }
    }
    let _ = client.disconnect().await;
}
