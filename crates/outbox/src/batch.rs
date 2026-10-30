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
