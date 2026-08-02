//! P5 — delegate (advisory; model-decision non-deterministic).

use std::time::{Duration, Instant};

use crate::client::DaemonClient;
use crate::config::ConformanceConfig;
use crate::paths::elapsed_ms;
use crate::result::{PathId, PathResult};

pub async fn run(client: &DaemonClient, cfg: &ConformanceConfig, timeout: Duration) -> PathResult {
    let start = Instant::now();
    let _ = cfg;

    // Prompt that strongly asks for delegation to a subagent.
    let turn = match client
        .chat_turn(
            "You must use the delegate tool to have a specialist look up today's date via tools \
             and report back. Do not answer from memory; call delegate with a clear goal.",
            true,
        )
        .await
    {
        Ok(t) => t,
        Err(e) => {
            return PathResult::fail(
                PathId::P5,
                "POST /api/chat/stream for delegate turn",
                elapsed_ms(start),
                serde_json::json!({"error": e}),
            );
        }
    };

    // Wait a bit for a child session to appear under sessions list.
    let deadline = start + timeout.min(Duration::from_secs(180));
    loop {
        if let Ok(sessions) = client.sessions().await
            && let Some(child) = find_background_child(&sessions, &turn.session_id)
        {
            return PathResult::pass(
                PathId::P5,
                "child background session exists under chat parent",
                elapsed_ms(start),
                serde_json::json!({
                    "parent": turn.session_id,
                    "child": child,
                }),
            );
        }
        if Instant::now() >= deadline {
            return PathResult::fail(
                PathId::P5,
                "child Background session with dispatcher grant (model may not have delegated)",
                elapsed_ms(start),
                serde_json::json!({
                    "parent": turn.session_id,
                    "note": "advisory path — model decision non-deterministic",
                }),
            );
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

fn find_background_child(sessions: &serde_json::Value, parent: &str) -> Option<serde_json::Value> {
    let arr = sessions.as_array()?;
    for s in arr {
        let vis = s.get("visibility").and_then(|v| v.as_str()).unwrap_or("");
        let parent_session = s
            .get("parent_session")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if vis == "background" && parent_session == parent {
            return Some(s.clone());
        }
    }
    None
}
