//! Durable reminder outbox.
//!
//! The file is `<data_dir>/liberado.db`. A `schema_meta` row leaves room for a later
//! mailbox in the same file. The worker is the only sender. Claim, accept, and fail
//! are single transactions.

mod ack;
mod batch;
mod cursor;
mod error;
mod keys;
mod pass;
mod plan;
mod rows;
mod store;
mod subs;
mod time;

pub use ack::constant_time_eq;
pub use batch::{DeliveryBatch, DeliveryOutcome, DeliveryRow};
pub use error::Error;
pub use keys::{content_id, fanout_key, task_key};
pub use pass::{DeliverySink, PassLimits, PassReport, run_pass};
pub use plan::{
    FanoutInput, PlannedTarget, TelegramMode, coalesce, fallback_reason, in_catchup_window,
    plan_targets,
};
pub use rows::{EnqueueOutcome, NewReminder, OutboxCounts, VisibleReminder};
pub use store::Outbox;
pub use subs::{NewSubscription, SubscriptionKeys, SubscriptionView};

#[cfg(test)]
mod fault_tests;

pub const DB_FILE: &str = "liberado.db";
