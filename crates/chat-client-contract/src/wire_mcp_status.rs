//! MCP rows on `GET /api/status`, kept out of `wire` so that file stays under
//! its physical-line review boundary.

use serde::{Deserialize, Serialize};

use crate::wire::DaemonStatus;

/// One MCP peer on `GET /api/status`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpPeerStatus {
    pub name: String,
    /// `"connected"` or `"degraded"`.
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(default)]
    pub attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_retry_ms: Option<u64>,
    #[serde(default)]
    pub tools: u32,
}

impl Default for DaemonStatus {
    fn default() -> Self {
        Self {
            running: false,
            vault_path: String::new(),
            uptime_seconds: 0,
            watcher_active: false,
            dispatcher_attached: false,
            orchestrator_attached: false,
            reactions_seen: 0,
            model_name: None,
            token_usage_total: None,
            context_window: None,
            chat_tools: 0,
            chat_tool_names: Vec::new(),
            enter_sends: true,
            mcp: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
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

    #[test]
    fn default_enter_sends_is_true_and_mcp_is_empty() {
        let status = DaemonStatus::default();
        assert!(status.enter_sends);
        assert!(status.mcp.is_empty());
    }
}
