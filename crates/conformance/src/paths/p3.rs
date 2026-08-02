//! P3 — hook → joinable session (may share P1b's machinery; runs its own trigger for isolation).

use std::time::{Duration, Instant};

use ulid::Ulid;

use crate::client::DaemonClient;
use crate::config::ConformanceConfig;
use crate::paths::elapsed_ms;
use crate::result::{PathId, PathResult};

pub async fn run(client: &DaemonClient, cfg: &ConformanceConfig, timeout: Duration) -> PathResult {
    let start = Instant::now();
    let secret = match cfg.hook_secret() {
        Ok(s) => s,
        Err(e) => return PathResult::skipped(PathId::P3, e),
    };

    let run_id = Ulid::new().to_string();
    let accept = match client
        .trigger_hook(&cfg.hook_name, &secret, &run_id)
        .await
    {
        Ok(v) => v,
        Err(e) => {
            return PathResult::fail(
                PathId::P3,
                "POST /api/hooks/conformance",
                elapsed_ms(start),
                serde_json::json!({"error": e}),
            );
        }
    };
    let correlation_id = accept
        .get("correlation_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let remaining = timeout.saturating_sub(start.elapsed());
    let session_id = match client.wait_dispatched(&correlation_id, remaining).await {
        Ok(id) => id,
        Err(e) => {
            return PathResult::fail(
                PathId::P3,
                "Dispatched session_id for correlation",
                elapsed_ms(start),
                serde_json::json!({"error": e, "correlation_id": correlation_id}),
            );
        }
    };

    if let Err(e) = client.goal(&session_id).await {
        return PathResult::fail(
            PathId::P3,
            "GET /api/goals/{id} returns the session",
            elapsed_ms(start),
            serde_json::json!({"error": e, "session_id": session_id}),
        );
    }

    if let Err(e) = client.goal_stream_accepts(&session_id).await {
        return PathResult::fail(
            PathId::P3,
            "GET /api/goals/{id}/stream accepts a subscriber",
            elapsed_ms(start),
            serde_json::json!({"error": e, "session_id": session_id}),
        );
    }

    PathResult::pass(
        PathId::P3,
        "hook session is joinable (get + stream)",
        elapsed_ms(start),
        serde_json::json!({
            "session_id": session_id,
            "correlation_id": correlation_id,
        }),
    )
}
