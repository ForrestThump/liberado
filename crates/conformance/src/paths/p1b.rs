//! P1b — event → dispatch → execute via conformance hook.

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
        Err(e) => return PathResult::skipped(PathId::P1b, e),
    };

    let run_id = Ulid::new().to_string();
    let accept = match client
        .trigger_hook(&cfg.hook_name, &secret, &run_id)
        .await
    {
        Ok(v) => v,
        Err(e) => {
            return PathResult::fail(
                PathId::P1b,
                "POST /api/hooks/conformance",
                elapsed_ms(start),
                serde_json::json!({"error": e, "run_id": run_id}),
            );
        }
    };

    let correlation_id = accept
        .get("correlation_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if correlation_id.is_empty() {
        return PathResult::fail(
            PathId::P1b,
            "hook response carries correlation_id",
            elapsed_ms(start),
            serde_json::json!({"body": accept, "run_id": run_id}),
        );
    }

    let remaining = timeout.saturating_sub(start.elapsed());
    let session_id = match client.wait_dispatched(&correlation_id, remaining).await {
        Ok(id) => id,
        Err(e) => {
            return PathResult::fail(
                PathId::P1b,
                "ReactionOutcome::Dispatched for correlation_id",
                elapsed_ms(start),
                serde_json::json!({"error": e, "correlation_id": correlation_id, "run_id": run_id}),
            );
        }
    };

    let remaining = timeout.saturating_sub(start.elapsed());
    let snap = match client.wait_goal_terminal(&session_id, remaining).await {
        Ok(s) => s,
        Err(e) => {
            return PathResult::fail(
                PathId::P1b,
                "goal session reaches terminal",
                elapsed_ms(start),
                serde_json::json!({"error": e, "session_id": session_id, "run_id": run_id}),
            );
        }
    };

    let status = snap
        .pointer("/session/status")
        .or_else(|| snap.get("status"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if status != "succeeded" {
        return PathResult::fail(
            PathId::P1b,
            "terminal status is succeeded (not merely terminal)",
            elapsed_ms(start),
            serde_json::json!({"status": status, "session_id": session_id, "run_id": run_id}),
        );
    }

    // Ground truth is the vault artifact — not ToolFinished on the hub event log.
    // The dispatch path records progress + session_finished, not tool_finished frames
    // (verified live 2026-08-01). Claiming ToolFinished without emitting it was suite theatre.
    let artifact = cfg
        .vault_path
        .join("conformance")
        .join("artifacts")
        .join(format!("{run_id}.md"));
    let body = match std::fs::read_to_string(&artifact) {
        Ok(b) => b,
        Err(e) => {
            return PathResult::fail(
                PathId::P1b,
                "artifact exists on disk under conformance zone",
                elapsed_ms(start),
                serde_json::json!({
                    "error": e.to_string(),
                    "path": artifact.display().to_string(),
                    "session_id": session_id,
                    "run_id": run_id,
                }),
            );
        }
    };
    let expected = format!("CONFORMANCE_OK {run_id}");
    if !body.contains(&expected) {
        return PathResult::fail(
            PathId::P1b,
            "artifact body contains CONFORMANCE_OK <run_id>",
            elapsed_ms(start),
            serde_json::json!({
                "path": artifact.display().to_string(),
                "body_preview": body.chars().take(200).collect::<String>(),
                "expected_substring": expected,
                "session_id": session_id,
                "run_id": run_id,
            }),
        );
    }

    PathResult::pass(
        PathId::P1b,
        "hook → dispatched → succeeded → artifact on disk",
        elapsed_ms(start),
        serde_json::json!({
            "correlation_id": correlation_id,
            "session_id": session_id,
            "run_id": run_id,
            "artifact": artifact.display().to_string(),
        }),
    )
}
