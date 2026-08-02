//! P4 — spawn under the conformance profile; grant must be the profile's, not domain fallback.

use std::time::{Duration, Instant};

use crate::client::DaemonClient;
use crate::config::ConformanceConfig;
use crate::paths::elapsed_ms;
use crate::result::{PathId, PathResult};

pub async fn run(client: &DaemonClient, cfg: &ConformanceConfig, _timeout: Duration) -> PathResult {
    let start = Instant::now();

    let body = serde_json::json!({
        "description": "conformance P4: finish immediately without tools; this only checks the grant stamp",
        "domain": "dispatch",
        "profile": cfg.profile_name,
        "success_criteria": [],
    });

    let accept = match client.start_goal(body).await {
        Ok(v) => v,
        Err(e) => {
            return PathResult::fail(
                PathId::P4,
                "POST /api/goals with conformance profile",
                elapsed_ms(start),
                serde_json::json!({"error": e, "profile": cfg.profile_name}),
            );
        }
    };

    let session_id = accept
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if session_id.is_empty() {
        return PathResult::fail(
            PathId::P4,
            "goals start returns session_id",
            elapsed_ms(start),
            serde_json::json!({"body": accept}),
        );
    }

    // Snapshot immediately — grant is stamped at start.
    let snap = match client.goal(&session_id).await {
        Ok(s) => s,
        Err(e) => {
            return PathResult::fail(
                PathId::P4,
                "GET /api/goals/{id}",
                elapsed_ms(start),
                serde_json::json!({"error": e, "session_id": session_id}),
            );
        }
    };

    let grant = snap
        .pointer("/session/grant")
        .or_else(|| snap.get("grant"))
        .cloned()
        .unwrap_or(serde_json::Value::Null);

    let profile_on_grant = grant.get("profile").and_then(|v| v.as_str()).unwrap_or("");

    if profile_on_grant != cfg.profile_name {
        return PathResult::fail(
            PathId::P4,
            "session.grant.profile equals the requested profile (not domain fallback)",
            elapsed_ms(start),
            serde_json::json!({
                "session_id": session_id,
                "expected_profile": cfg.profile_name,
                "grant_profile": profile_on_grant,
                "grant": grant,
            }),
        );
    }

    // Distinguishable from an empty / domain-fallback grant: must have some capability.
    let caps = grant
        .get("capabilities")
        .or_else(|| grant.pointer("/capabilities"));
    let cap_empty = match caps {
        Some(serde_json::Value::Array(a)) => a.is_empty(),
        Some(serde_json::Value::Object(m)) => m.is_empty(),
        None => true,
        _ => false,
    };
    if cap_empty {
        return PathResult::fail(
            PathId::P4,
            "profile grant is non-empty (distinguishable from powerless domain fallback)",
            elapsed_ms(start),
            serde_json::json!({
                "session_id": session_id,
                "grant": grant,
            }),
        );
    }

    PathResult::pass(
        PathId::P4,
        "spawned session carries the conformance profile grant",
        elapsed_ms(start),
        serde_json::json!({
            "session_id": session_id,
            "grant_profile": profile_on_grant,
            "grant": grant,
        }),
    )
}
