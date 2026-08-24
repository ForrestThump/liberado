//! Split from `trace.rs`: kills the baseline campaign's survivors.
//!
//! Covers session-id sanitisation and the turn tracer's event recording,
//! including the verbatim token mirror onto the live bus.

use super::*;
use liberado_executor::{TurnObserver, TurnRecord};

#[test]
fn safe_segments_pass_through() {
    assert_eq!(safe_segment("abc_123"), "abc_123");
}

#[test]
fn unsafe_characters_become_inner_dashes() {
    assert_eq!(safe_segment("a/b c.d"), "a-b-c-d");
    assert_eq!(safe_segment("task:id"), "task-id");
}

#[test]
fn underscores_survive_sanitisation() {
    assert_eq!(safe_segment("a_b"), "a_b");
}

#[test]
fn leading_and_trailing_dashes_are_trimmed() {
    assert_eq!(safe_segment("-abc-"), "abc");
}

#[test]
fn an_empty_segment_falls_back_to_session() {
    assert_eq!(safe_segment(""), "session");
    assert_eq!(safe_segment("///"), "session", "all-dashes trim to nothing");
}

#[test]
fn session_id_prefixes_the_sanitised_task_id() {
    let request: CoderRunRequest = serde_json::from_value(serde_json::json!({
        "task": {"id": "my/task:7", "description": "d"},
        "workspace": {"root": "/tmp/ws", "base_ref": "main"},
        "config": {
            "backend": "loop",
            "planner": {"model": "p"},
            "coder": {"model": "c"},
            "critic": {"model": "cr"}
        }
    }))
    .expect("request json");
    let id = session_id(&request);
    assert!(
        id.starts_with("my-task-7-attempt-"),
        "unsafe characters must not reach the filesystem: {id}"
    );
}

fn record(content: Option<&str>) -> TurnRecord {
    TurnRecord {
        turn: 3,
        tools_offered: vec!["read_file".into()],
        message_count: 9,
        content: content.map(str::to_string),
        finish_reason: "prose",
        tool_calls: vec![],
        prompt_tokens: 11,
        completion_tokens: 7,
    }
}

#[test]
fn every_turn_lands_in_the_trace_log() {
    let log: EventLog = Arc::new(Mutex::new(Vec::new()));
    let tracer = TurnTracer::new(Arc::clone(&log), "worker");
    tracer.on_turn(record(Some("hello world")));
    let events = log.lock().expect("lock");
    assert_eq!(events.len(), 1, "{events:?}");
    match &events[0] {
        CoderEvent::ModelTurnFinished {
            role,
            turn,
            content,
            prompt_tokens,
            completion_tokens,
            ..
        } => {
            assert_eq!(role, "worker");
            assert_eq!(*turn, 3);
            assert_eq!(content.as_deref(), Some("hello world"));
            assert_eq!(*prompt_tokens, 11);
            assert_eq!(*completion_tokens, 7);
        }
        other => panic!("expected ModelTurnFinished, got {other:?}"),
    }
}

/// Non-empty turn text is mirrored onto the live bus as a Token — the producer
/// side every consumer used to wait for in vain.
#[tokio::test]
async fn prose_is_mirrored_to_the_live_bus() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    let log: EventLog = Arc::new(Mutex::new(Vec::new()));
    let tracer = TurnTracer::new(Arc::clone(&log), "worker");
    crate::live::with_live_events(tx, "s1", async {
        tracer.on_turn(record(Some("spoken prose")));
    })
    .await;
    let event = rx.recv().await.expect("token event");
    match event.kind {
        liberado_session::SessionEventKind::Token { text } => {
            assert_eq!(text, "spoken prose");
        }
        other => panic!("expected Token, got {other:?}"),
    }
}

/// Blank turn text must stay off the live bus. Whitespace-only turns are the
/// common shape when a model answers with tool calls plus stray padding.
#[tokio::test]
async fn blank_prose_is_not_mirrored_to_the_live_bus() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    let log: EventLog = Arc::new(Mutex::new(Vec::new()));
    let tracer = TurnTracer::new(Arc::clone(&log), "worker");
    crate::live::with_live_events(tx, "s1", async {
        tracer.on_turn(record(Some("   \n\t")));
    })
    .await;
    let mut tokens = 0;
    while let Ok(event) = rx.try_recv() {
        if matches!(event.kind, liberado_session::SessionEventKind::Token { .. }) {
            tokens += 1;
        }
    }
    assert_eq!(tokens, 0, "blank padding must not become a Token");
}
