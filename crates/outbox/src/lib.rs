//! Durable reminder outbox.
//!
//! The file is `<data_dir>/liberado.db`. A `schema_meta` row leaves room for a later
//! mailbox in the same file. The worker is the only sender. Claim, accept, and fail
//! are single transactions.

mod ack;
mod auth_store;
mod batch;
mod completion_diff;
mod completion_time;
mod completions;
pub mod copy;
mod cursor;
mod error;
mod keys;
mod kind;
mod migrate;
mod pass;
mod plan;
mod resolution;
mod row_detail;
mod row_open;
mod rows;
mod source;
mod store;
mod subs;
mod task_line;
mod time;

pub use ack::constant_time_eq;
pub use auth_store::{
    PasskeyCredential, PasskeyRow, SESSION_ABSOLUTE, SESSION_IDLE, SessionRecord, hash_session_id,
};
pub use batch::{DeliveryBatch, DeliveryOutcome, DeliveryRow};
pub use completion_diff::{SnapshotChange, TaskCounts, diff_snapshot};
pub use completion_time::{
    CompletionTiming, DoneHint, PRECISION_DATE, PRECISION_TAP, PRECISION_TICK, SOURCE_LIBERADO,
    SOURCE_OBSIDIAN, completion_stamp, completion_timing, local_date_of, local_noon_utc,
};
pub use completions::{
    CompletionDraft, DONE_HINT_MINUTES, NewCompletion, NoteApply, TaskCompletion, VoidDraft,
    content_fingerprint, liberado_tap_completion,
};
pub use copy::DigestItem;
pub use error::Error;
pub use keys::{content_id, fanout_key, key_hash_suffix, task_key};
pub use kind::{
    DIGEST, EVENT_LIST, EVENT_START, HABIT, JOB_ERROR, REMINDER, TASK_DUE, TASK_LIST, TEST,
};
pub use pass::{DeliverySink, PassLimits, PassReport, run_pass};
pub use plan::{
    FanoutInput, OpenRow, PlannedTarget, TelegramMode, coalesce, fallback_reason,
    in_catchup_window, plan_targets,
};
pub use resolution::{
    OwnerResolveResult, OwnerSnoozeResult, RES_COMPLETED, RES_DISMISSED, RES_SNOOZED,
};
pub use row_detail::OutboxRecord;
pub use rows::{EnqueueOutcome, NewReminder, OutboxCounts, VisibleReminder};
pub use source::OutboxSource;
pub use store::Outbox;
pub use subs::{NewSubscription, SubscriptionKeys, SubscriptionView, WeekTarget};
pub use task_line::{
    collapse_whitespace, line_is_recurring, strip_task_metadata, task_identity_key,
};

#[cfg(test)]
mod fault_tests;

pub const DB_FILE: &str = "liberado.db";
