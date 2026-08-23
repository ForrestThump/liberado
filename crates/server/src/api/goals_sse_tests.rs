//! Split from `goals.rs` for module-health boundaries.

use super::*;
use liberado_session::{SessionEvent, SessionEventKind};

fn event(kind: SessionEventKind) -> axum::response::sse::Event {
    session_event_to_sse(&SessionEvent {
        session_id: "s1".into(),
        at: chrono::Utc::now(),
        kind,
    })
}

#[test]
fn session_event_to_sse_maps_every_kind_to_its_name() {
    let cases: Vec<(SessionEventKind, &str)> = vec![
        (
            SessionEventKind::SessionStarted {
                domain: "coding".into(),
                description: "d".into(),
            },
            "session_started",
        ),
        (
            SessionEventKind::RoleStarted {
                role: "r".into(),
                model: "m".into(),
            },
            "role_started",
        ),
        (
            SessionEventKind::RoleFinished { role: "r".into() },
            "role_finished",
        ),
        (
            SessionEventKind::ToolStarted {
                name: "t".into(),
                args_preview: "a".into(),
            },
            "tool_started",
        ),
        (
            SessionEventKind::ToolFinished {
                name: "t".into(),
                ok: true,
                result_preview: "r".into(),
            },
            "tool_finished",
        ),
        (
            SessionEventKind::Progress {
                message: "p".into(),
            },
            "progress",
        ),
        (
            SessionEventKind::AwaitingInput {
                prompt: "p".into(),
                options: Vec::new(),
            },
            "awaiting_input",
        ),
        (
            SessionEventKind::HumanInput { text: "h".into() },
            "human_input",
        ),
        (
            SessionEventKind::ValidationFinished {
                ok: true,
                summary: "s".into(),
            },
            "validation_finished",
        ),
        (
            SessionEventKind::CriticVerdict {
                reviewer: "rv".into(),
                kind: "k".into(),
                approved: true,
                issues: Vec::new(),
                coerced: false,
            },
            "critic_verdict",
        ),
        (
            SessionEventKind::FileChanged {
                path: "f".into(),
                change: "c".into(),
            },
            "file_changed",
        ),
        (
            SessionEventKind::Checkpoint {
                id: "c1".into(),
                label: "l".into(),
                tree_hash: "h".into(),
            },
            "checkpoint",
        ),
        (
            SessionEventKind::LoopGuard {
                guard: "g".into(),
                action: "a".into(),
            },
            "loop_guard",
        ),
        (
            SessionEventKind::SessionFinished {
                status: "ok".into(),
                summary: "s".into(),
            },
            "session_finished",
        ),
        (
            SessionEventKind::Failed {
                message: "m".into(),
            },
            "failed",
        ),
    ];
    for (kind, expected) in cases {
        let ev = event(kind);
        let dbg = format!("{ev:?}");
        assert!(dbg.contains(&format!("event: {expected}\\n")), "{dbg}");
    }
}

#[test]
fn session_event_to_sse_token_is_a_bare_text_frame() {
    let ev = event(SessionEventKind::Token {
        text: "hello".into(),
    });
    let dbg = format!("{ev:?}");
    assert!(dbg.contains("event: token\\n"), "{dbg}");
    assert!(dbg.contains("data: hello"), "{dbg}");
}

#[test]
fn session_event_to_sse_carries_the_full_event_json() {
    let ev = event(SessionEventKind::RoleStarted {
        role: "orchestrator".into(),
        model: "m".into(),
    });
    let dbg = format!("{ev:?}");
    assert!(dbg.contains("orchestrator"), "{dbg}");
    assert!(dbg.contains(r#"\"type\":\"role_started\""#), "{dbg}");
}
