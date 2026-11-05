//! Where a reminder row came from — persisted as JSON on the outbox row.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OutboxSource {
    VaultNote {
        path: String,
        label: String,
        text: String,
    },
    Schedule {
        name: String,
    },
    Settings,
}

impl OutboxSource {
    pub fn encode(row: &Option<Self>) -> Option<String> {
        row.as_ref()
            .and_then(|source| serde_json::to_string(source).ok())
    }

    pub fn decode(text: Option<String>) -> Option<Self> {
        let text = text?;
        serde_json::from_str(&text).ok()
    }
}
