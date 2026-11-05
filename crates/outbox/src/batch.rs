use chrono::{DateTime, Utc};

use crate::copy::DigestItem;
use crate::source::OutboxSource;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DeliveryRow {
    pub id: i64,
    pub token: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryBatch {
    pub channel: String,
    pub target: String,
    pub title: String,
    pub body: String,
    pub tag: String,
    pub rows: Vec<DeliveryRow>,
    pub kind: String,
    pub outbox_id: Option<i64>,
    pub due_at: Option<DateTime<Utc>>,
    pub late: bool,
    pub source: Option<OutboxSource>,
    pub items: Vec<DigestItem>,
}

impl DeliveryBatch {
    pub fn ids(&self) -> Vec<i64> {
        self.rows.iter().map(|row| row.id).collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryOutcome {
    Accepted,
    Gone,
    Retryable(String),
    Unconfigured(String),
}
