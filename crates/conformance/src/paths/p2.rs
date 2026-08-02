//! P2 — chat turn (background).

use std::time::{Duration, Instant};

use crate::client::DaemonClient;
use crate::config::ConformanceConfig;
use crate::paths::elapsed_ms;
use crate::result::{PathId, PathResult};

pub async fn run(client: &DaemonClient, cfg: &ConformanceConfig, _timeout: Duration) -> PathResult {
    let start = Instant::now();
    let status = match client.status().await {
        Ok(s) => s,
        Err(e) => {
            return PathResult::fail(
                PathId::P2,
                "GET /api/status",
                elapsed_ms(start),
                serde_json::json!({"error": e}),
            );
        }
    };
    let active_model = status.model_name.clone();

    let turn = match client
        .chat_turn(
            "Reply with exactly the single word pong and nothing else.",
            true, // background — out of sidebar
        )
        .await
    {
        Ok(t) => t,
        Err(e) => {
            return PathResult::fail(
                PathId::P2,
                "POST /api/chat/stream",
                elapsed_ms(start),
                serde_json::json!({"error": e}),
            );
        }
    };

    if !turn.saw_token {
        return PathResult::fail(
            PathId::P2,
            "at least one Token delta (provider reached)",
            elapsed_ms(start),
            serde_json::json!({"session_id": turn.session_id}),
        );
    }

    // Background chats are filtered from GET /api/conversations — fetch by id and via sessions.
    let conv = match client.conversation(&turn.session_id).await {
        Ok(c) => c,
        Err(e) => {
            return PathResult::fail(
                PathId::P2,
                "GET /api/conversations/{id}",
                elapsed_ms(start),
                serde_json::json!({"error": e, "session_id": turn.session_id}),
            );
        }
    };

    let (has_user, has_assistant, assistant_model) = inspect_transcript(&conv);
    if !has_user || !has_assistant {
        return PathResult::fail(
            PathId::P2,
            "transcript has User and Assistant nodes",
            elapsed_ms(start),
            serde_json::json!({
                "session_id": turn.session_id,
                "has_user": has_user,
                "has_assistant": has_assistant,
            }),
        );
    }

    // Visibility must be background (sessions list).
    let visibility = session_visibility(client, &turn.session_id).await;
    if visibility.as_deref() != Some("background") {
        return PathResult::fail(
            PathId::P2,
            "session visibility is background",
            elapsed_ms(start),
            serde_json::json!({
                "session_id": turn.session_id,
                "visibility": visibility,
            }),
        );
    }

    // Model cross-check when both sides present.
    if let (Some(active), Some(stamped)) = (active_model.as_ref(), assistant_model.as_ref())
        && active != stamped
    {
        return PathResult::fail(
            PathId::P2,
            "assistant MessageNode.model equals daemon active model",
            elapsed_ms(start),
            serde_json::json!({
                "active_model": active,
                "assistant_model": stamped,
                "session_id": turn.session_id,
            }),
        );
    }

    let _ = cfg; // vault not needed for P2
    PathResult::pass(
        PathId::P2,
        "background chat turn: tokens, transcript, model stamp",
        elapsed_ms(start),
        serde_json::json!({
            "session_id": turn.session_id,
            "active_model": active_model,
            "assistant_model": assistant_model,
            "visibility": visibility,
        }),
    )
}

fn inspect_transcript(conv: &serde_json::Value) -> (bool, bool, Option<String>) {
    let nodes = conv
        .get("nodes")
        .or_else(|| conv.get("messages"))
        .or_else(|| conv.get("history"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let mut has_user = false;
    let mut has_assistant = false;
    let mut assistant_model = None;

    for n in &nodes {
        let role = n
            .pointer("/message/role")
            .or_else(|| n.get("role"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let author = n.get("author").and_then(|v| v.as_str()).unwrap_or("");
        if role == "user" || author == "user" {
            has_user = true;
        }
        if role == "assistant" || author == "assistant" {
            has_assistant = true;
            if assistant_model.is_none() {
                assistant_model = n
                    .get("model")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
            }
        }
    }
    (has_user, has_assistant, assistant_model)
}

async fn session_visibility(client: &DaemonClient, id: &str) -> Option<String> {
    let sessions = client.sessions().await.ok()?;
    let arr = sessions.as_array()?;
    for s in arr {
        let sid = s.get("id").and_then(|v| v.as_str()).unwrap_or("");
        if sid == id {
            return s
                .get("visibility")
                .and_then(|v| v.as_str())
                .map(str::to_string);
        }
    }
    None
}
