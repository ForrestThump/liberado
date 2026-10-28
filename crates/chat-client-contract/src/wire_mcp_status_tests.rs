//! Serde for the MCP rows on `GET /api/status`.
//!
//! Split from `wire.rs` so that file does not gain a function.

use super::*;

#[test]
fn absent_mcp_defaults_to_an_empty_list() {
    let json = serde_json::json!({
        "running": true,
        "vault_path": "/v",
        "watcher_active": true,
        "dispatcher_attached": false,
        "orchestrator_attached": false,
        "reactions_seen": 0,
    });
    let status: DaemonStatus = serde_json::from_value(json).unwrap();
    assert!(status.mcp.is_empty());
}

#[test]
fn mcp_peer_status_roundtrip_keeps_degraded_fields() {
    let row = McpPeerStatus {
        name: "turbovault".into(),
        state: "degraded".into(),
        last_error: Some("peer down".into()),
        attempts: 3,
        next_retry_ms: Some(4_000),
        tools: 0,
    };
    let json = serde_json::to_value(&row).unwrap();
    assert_eq!(json["name"], "turbovault");
    assert_eq!(json["state"], "degraded");
    assert_eq!(json["attempts"], 3);
    assert_eq!(json["next_retry_ms"], 4_000);
    assert_eq!(json["tools"], 0);
    let back: McpPeerStatus = serde_json::from_value(json).unwrap();
    assert_eq!(back, row);
}

#[test]
fn connected_row_omits_empty_optional_fields() {
    let row = McpPeerStatus {
        name: "turbovault".into(),
        state: "connected".into(),
        last_error: None,
        attempts: 0,
        next_retry_ms: None,
        tools: 67,
    };
    let json = serde_json::to_value(&row).unwrap();
    assert!(json.get("last_error").is_none());
    assert!(json.get("next_retry_ms").is_none());
    assert_eq!(json["tools"], 67);
}
