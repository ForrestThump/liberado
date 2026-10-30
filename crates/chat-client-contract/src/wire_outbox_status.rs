//! Outbox queue counters on `GET /api/status`, kept out of `wire` so that file stays under
//! its physical-line review boundary.

use serde::{Deserialize, Serialize};

/// Reminder outbox depth on `GET /api/status` (flattened into [`crate::wire::DaemonStatus`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct DaemonOutboxStatus {
    #[serde(default)]
    pub outbox_pending: u64,
    #[serde(default)]
    pub outbox_failed: u64,
    #[serde(default)]
    pub outbox_dead: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outbox_oldest_pending_secs: Option<u64>,
}

#[cfg(test)]
mod tests {
    use crate::wire::DaemonStatus;

    #[test]
    fn absent_outbox_counters_default_to_zero() {
        let json = serde_json::json!({
            "running": true,
            "vault_path": "/v",
            "watcher_active": true,
            "dispatcher_attached": false,
            "orchestrator_attached": false,
            "reactions_seen": 0,
        });
        let status: DaemonStatus = serde_json::from_value(json).unwrap();
        assert_eq!(status.outbox.outbox_pending, 0);
        assert_eq!(status.outbox.outbox_failed, 0);
        assert_eq!(status.outbox.outbox_dead, 0);
        assert!(status.outbox.outbox_oldest_pending_secs.is_none());
    }
}
