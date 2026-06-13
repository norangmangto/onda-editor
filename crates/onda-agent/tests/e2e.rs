//! End-to-end conformance tests driving the real `onda-mock-agent` subprocess
//! over the NDJSON transport (onda T22.0/T22.2). These exercise the full path:
//! spawn → handshake → prompt → streaming → agent→client requests → teardown.

use std::time::Duration;

use onda_agent::protocol::PermissionOutcome;
use onda_agent::{AgentClient, AgentConfig, AgentEvent, StopReason};
use tokio::sync::mpsc;

const MOCK: &str = env!("CARGO_BIN_EXE_onda-mock-agent");

fn config(scenario: &str) -> AgentConfig {
    AgentConfig {
        name: "mock".into(),
        command: MOCK.into(),
        args: vec![],
        env: vec![("ONDA_MOCK_SCENARIO".into(), scenario.into())],
    }
}

async fn next_event(rx: &mut mpsc::Receiver<AgentEvent>) -> AgentEvent {
    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for agent event")
        .expect("agent event channel closed")
}

/// Drain events until `SessionCreated`, returning the client.
async fn connect(scenario: &str) -> (AgentClient, mpsc::Receiver<AgentEvent>) {
    let (tx, mut rx) = mpsc::channel(64);
    let cwd = std::env::current_dir().unwrap();
    let client = AgentClient::connect(&config(scenario), cwd, tx)
        .await
        .expect("connect");
    // Handshake: Initialized then SessionCreated.
    loop {
        match next_event(&mut rx).await {
            AgentEvent::SessionCreated { .. } => break,
            AgentEvent::Initialized { .. } => continue,
            other => panic!("unexpected handshake event: {other:?}"),
        }
    }
    (client, rx)
}

#[tokio::test]
async fn stream_scenario_streams_and_ends() {
    let (client, mut rx) = connect("stream").await;
    client
        .prompt(vec![onda_agent::ContentBlock::text("hi")])
        .await
        .unwrap();

    let mut chunks = Vec::new();
    loop {
        match next_event(&mut rx).await {
            AgentEvent::MessageChunk { text } => chunks.push(text),
            AgentEvent::Plan(entries) => assert_eq!(entries[0].content, "do the thing"),
            AgentEvent::TurnEnded { stop_reason } => {
                assert_eq!(stop_reason, StopReason::EndTurn);
                break;
            }
            other => panic!("unexpected: {other:?}"),
        }
    }
    assert_eq!(chunks.join(""), "Hello world");
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn tool_call_lifecycle_streams() {
    let (client, mut rx) = connect("tool").await;
    client
        .prompt(vec![onda_agent::ContentBlock::text("edit")])
        .await
        .unwrap();

    let mut saw_start = false;
    let mut saw_complete = false;
    loop {
        match next_event(&mut rx).await {
            AgentEvent::ToolCallStarted(tc) => {
                assert_eq!(tc.tool_call_id, "t1");
                saw_start = true;
            }
            AgentEvent::ToolCallUpdated(u) => {
                if u.status == Some(onda_agent::ToolCallStatus::Completed) {
                    saw_complete = true;
                }
            }
            AgentEvent::TurnEnded { .. } => break,
            AgentEvent::MessageChunk { .. } => {}
            other => panic!("unexpected: {other:?}"),
        }
    }
    assert!(saw_start && saw_complete);
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn permission_request_round_trips() {
    let (client, mut rx) = connect("permission").await;
    client
        .prompt(vec![onda_agent::ContentBlock::text("run")])
        .await
        .unwrap();

    let mut allowed_echo = None;
    loop {
        match next_event(&mut rx).await {
            AgentEvent::PermissionRequest { request_id, params } => {
                assert_eq!(params.options[0].option_id, "allow");
                client
                    .respond_permission(
                        request_id,
                        PermissionOutcome::Selected {
                            option_id: "allow".into(),
                        },
                    )
                    .await
                    .unwrap();
            }
            AgentEvent::MessageChunk { text } if text.starts_with("permission:") => {
                allowed_echo = Some(text);
            }
            AgentEvent::TurnEnded { .. } => break,
            AgentEvent::MessageChunk { .. } => {}
            other => panic!("unexpected: {other:?}"),
        }
    }
    assert_eq!(allowed_echo.as_deref(), Some("permission:allow"));
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn file_read_served_from_buffer_state() {
    // The agent must see (possibly unsaved) buffer content, not disk.
    let (client, mut rx) = connect("fileread").await;
    client
        .prompt(vec![onda_agent::ContentBlock::text("read")])
        .await
        .unwrap();

    let mut echoed = None;
    loop {
        match next_event(&mut rx).await {
            AgentEvent::FileReadRequest { request_id, params } => {
                assert_eq!(params.path, "src/main.rs");
                client
                    .respond_file_read(request_id, Ok("DIRTY_BUFFER".into()))
                    .await
                    .unwrap();
            }
            AgentEvent::MessageChunk { text } if text.starts_with("file:") => {
                echoed = Some(text);
            }
            AgentEvent::TurnEnded { .. } => break,
            AgentEvent::MessageChunk { .. } => {}
            other => panic!("unexpected: {other:?}"),
        }
    }
    assert_eq!(echoed.as_deref(), Some("file:DIRTY_BUFFER"));
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn malformed_line_is_isolated() {
    let (client, mut rx) = connect("malformed").await;
    client
        .prompt(vec![onda_agent::ContentBlock::text("x")])
        .await
        .unwrap();

    let mut saw_malformed = false;
    loop {
        match next_event(&mut rx).await {
            AgentEvent::Malformed(_) => saw_malformed = true,
            // Reaching TurnEnded proves the turn completes despite the garbage line.
            AgentEvent::TurnEnded { .. } => break,
            _ => {}
        }
    }
    assert!(saw_malformed, "malformed line should surface");
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn agent_death_mid_stream_reports_disconnect() {
    let (client, mut rx) = connect("die").await;
    client
        .prompt(vec![onda_agent::ContentBlock::text("x")])
        .await
        .unwrap();

    let mut disconnected = false;
    while let Ok(Some(ev)) = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
        if let AgentEvent::Error { message } = ev {
            if message.contains("disconnected") {
                disconnected = true;
                break;
            }
        }
    }
    assert!(
        disconnected,
        "agent death should surface a disconnect error"
    );
    let _ = client.shutdown().await;
}
