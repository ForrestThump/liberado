//! Durable reminders: enqueue into the outbox and run the delivery worker.

mod cursor;
mod delivery;
#[cfg(test)]
#[path = "delivery_tests.rs"]
mod delivery_tests;
mod enqueue;
#[cfg(test)]
#[path = "enqueue_tests.rs"]
mod enqueue_tests;

pub use cursor::OutboxScheduleCursor;
pub use delivery::{OutboxDelivery, TelegramReminderSender};
pub use enqueue::{
    MechanicalReminder, enqueue_mechanical, ensure_schedule_cursor, record_schedule_fire,
    spawn_worker,
};
