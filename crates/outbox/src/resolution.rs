//! Owner resolution state on outbox rows (complete / dismiss / snooze).

pub const RES_COMPLETED: &str = "completed";
pub const RES_DISMISSED: &str = "dismissed";
pub const RES_SNOOZED: &str = "snoozed";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnerResolveResult {
    Applied,
    AlreadyResolved(String),
    NotFound,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnerSnoozeResult {
    Applied { follow_up_id: i64 },
    AlreadyResolved(String),
    NotFound,
}
