//! Durable reminder outbox.
//!
//! The file is `<data_dir>/liberado.db`. A `schema_meta` row leaves room for a later
//! mailbox in the same file. The worker is the only sender. Claim, accept, and fail
//! are single transactions.

mod ack;
mod auth_store;
mod batch;
pub mod copy;
mod cursor;
mod error;
mod keys;
mod kind;
mod migrate;
mod pass;
mod plan;
mod row_open;
mod rows;
mod source;
mod store;
mod subs;
mod time;

pub use ack::constant_time_eq;
pub use auth_store::{
    PasskeyCredential, PasskeyRow, SESSION_ABSOLUTE, SESSION_IDLE, SessionRecord, hash_session_id,
};
pub use batch::{DeliveryBatch, DeliveryOutcome, DeliveryRow};
pub use copy::DigestItem;
pub use error::Error;
pub use keys::{content_id, fanout_key, task_key};
pub use kind::{
    DIGEST, EVENT_LIST, EVENT_START, HABIT, JOB_ERROR, REMINDER, TASK_DUE, TASK_LIST, TEST,
};
pub use pass::{DeliverySink, PassLimits, PassReport, run_pass};
pub use plan::{
    FanoutInput, PlannedTarget, TelegramMode, coalesce, fallback_reason, in_catchup_window,
    plan_targets,
};
pub use rows::{EnqueueOutcome, NewReminder, OutboxCounts, VisibleReminder};
pub use source::OutboxSource;
pub use store::Outbox;
pub use subs::{NewSubscription, SubscriptionKeys, SubscriptionView};

#[cfg(test)]
mod fault_tests;

pub const DB_FILE: &str = "liberado.db";
