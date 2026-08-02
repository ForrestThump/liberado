//! P6 — a turn outlives its connection (durable turn + attach + cancel rollback).
//!
//! Against the live daemon (or a forced-fail mock):
//!
//! 1. start a **background** chat turn, drop the SSE stream after the session id is announced;
//! 2. assert `turn_running` stays true with nobody attached;
//! 3. **attach** and observe stream content (replay/live — same SSE vocabulary);
//! 4. when finished, assert the assistant reply is on the transcript (**GET history**, not only SSE);
//! 5. separately: start a turn, **cancel**, assert it stops **and** the transcript has the question
//!    with **no** assistant reply (real rollback).
//!
//! Envelope: background only; only ids this run creates; no foreign cancels.

use std::time::{Duration, Instant};

use crate::client::DaemonClient;
use crate::config::ConformanceConfig;
use crate::paths::elapsed_ms;
use crate::result::{PathId, PathResult};

/// Prompt for the durable/outlive arm: long enough that disconnect+running is observable on a live box.
const DURABLE_PROMPT: &str = "Write a careful multi-paragraph explanation of why durable chat turns \
must outlive their HTTP connection. Use at least five short paragraphs. Do not refuse.";

const CANCEL_PROMPT: &str = "Begin a long numbered list from 1 to 200 of mundane household tasks. \
Do not stop early.";

pub async fn run(client: &DaemonClient, cfg: &ConformanceConfig, timeout: Duration) -> PathResult {
    let start = Instant::now();
    let _ = cfg;

    // ── A. Outlive disconnect + attach + reply on disk ─────────────────────────
    let session = match client
        .start_background_turn_drop_stream(DURABLE_PROMPT)
        .await
    {
        Ok(id) => id,
        Err(e) => {
            return PathResult::fail(
                PathId::P6,
                "POST /api/chat/stream background, drop after session id",
                elapsed_ms(start),
                serde_json::json!({"error": e}),
            );
        }
    };

    // Nobody attached: we dropped the only stream. Turn must still be running.
    let running_wait = timeout.min(Duration::from_secs(30));
    if let Err(e) = client.wait_turn_running(&session, running_wait).await {
        return PathResult::fail(
            PathId::P6,
            "turn_running true after stream drop (durable turn outlives connection)",
            elapsed_ms(start),
            serde_json::json!({"error": e, "session_id": session}),
        );
    }

    let attach_timeout = timeout.min(Duration::from_secs(60));
    let attach = match client.attach_and_collect(&session, attach_timeout).await {
        Ok(a) => a,
        Err(e) => {
            return PathResult::fail(
                PathId::P6,
                "GET /api/conversations/{id}/attach while turn_running",
                elapsed_ms(start),
                serde_json::json!({"error": e, "session_id": session}),
            );
        }
    };
    // Session framing alone is not enough — attach always emits `event: session` first.
    if !attach.has_turn_content() {
        return PathResult::fail(
            PathId::P6,
            "attach stream delivers turn content (token/replay), not only session framing",
            elapsed_ms(start),
            serde_json::json!({
                "session_id": session,
                "event_blocks": attach.event_blocks,
                "session_frames": attach.session_frames,
                "saw_token": attach.saw_token,
            }),
        );
    }

    let finish_wait = timeout
        .saturating_sub(start.elapsed())
        .max(Duration::from_secs(30));
    if let Err(e) = client.wait_turn_not_running(&session, finish_wait).await {
        return PathResult::fail(
            PathId::P6,
            "turn eventually finishes after attach",
            elapsed_ms(start),
            serde_json::json!({"error": e, "session_id": session}),
        );
    }

    let snap = match client.conversation_snapshot(&session).await {
        Ok(s) => s,
        Err(e) => {
            return PathResult::fail(
                PathId::P6,
                "GET /api/conversations/{id} after finish (disk ground truth)",
                elapsed_ms(start),
                serde_json::json!({"error": e, "session_id": session}),
            );
        }
    };
    if !snap.has_assistant {
        return PathResult::fail(
            PathId::P6,
            "assistant reply present on transcript after finish (not only SSE done)",
            elapsed_ms(start),
            serde_json::json!({
                "session_id": session,
                "has_user": snap.has_user,
                "has_assistant": snap.has_assistant,
                "turn_running": snap.turn_running,
            }),
        );
    }
    if !snap.has_user {
        return PathResult::fail(
            PathId::P6,
            "user message present on transcript",
            elapsed_ms(start),
            serde_json::json!({"session_id": session}),
        );
    }

    // ── B. Cancel leaves question, no reply ────────────────────────────────────
    let cancel_session = match client
        .start_background_turn_drop_stream(CANCEL_PROMPT)
        .await
    {
        Ok(id) => id,
        Err(e) => {
            return PathResult::fail(
                PathId::P6,
                "POST /api/chat/stream for cancel arm",
                elapsed_ms(start),
                serde_json::json!({"error": e, "outlive_session": session}),
            );
        }
    };

    // Best-effort: wait briefly for the turn to register; cancel still valid if already done
    // (rollback assert would then need an empty assistant — live race).
    let _ = client
        .wait_turn_running(&cancel_session, Duration::from_secs(10))
        .await;

    if let Err(e) = client.cancel_conversation(&cancel_session).await {
        return PathResult::fail(
            PathId::P6,
            "POST /api/conversations/{id}/cancel",
            elapsed_ms(start),
            serde_json::json!({"error": e, "session_id": cancel_session}),
        );
    }

    // Settle so a racy partial persist would still be visible.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let after_cancel = match client.conversation_snapshot(&cancel_session).await {
        Ok(s) => s,
        Err(e) => {
            return PathResult::fail(
                PathId::P6,
                "GET conversation after cancel",
                elapsed_ms(start),
                serde_json::json!({"error": e, "session_id": cancel_session}),
            );
        }
    };

    if after_cancel.turn_running {
        return PathResult::fail(
            PathId::P6,
            "cancel stops the turn (turn_running false)",
            elapsed_ms(start),
            serde_json::json!({
                "session_id": cancel_session,
                "turn_running": true,
            }),
        );
    }

    // Ground truth rollback: not merely "stopped" — transcript has question, no assistant body.
    if !after_cancel.cancel_left_question_without_reply() {
        return PathResult::fail(
            PathId::P6,
            "cancel persists nothing: user question kept, no assistant reply on transcript",
            elapsed_ms(start),
            serde_json::json!({
                "session_id": cancel_session,
                "has_user": after_cancel.has_user,
                "has_assistant": after_cancel.has_assistant,
                "assistant_contents": after_cancel.assistant_contents,
                "turn_unanswered": after_cancel.turn_unanswered,
            }),
        );
    }

    PathResult::pass(
        PathId::P6,
        "durable outlive + attach content + disk reply; cancel leaves question only",
        elapsed_ms(start),
        serde_json::json!({
            "outlive_session": session,
            "cancel_session": cancel_session,
            "attach_event_blocks": attach.event_blocks,
            "attach_session_frames": attach.session_frames,
            "attach_saw_token": attach.saw_token,
            "assistant_preview": snap.assistant_contents.first().map(|s| {
                let t: String = s.chars().take(120).collect();
                t
            }),
            "residue": "two background conversations created by this run only",
        }),
    )
}
