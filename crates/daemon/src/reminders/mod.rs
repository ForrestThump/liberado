//! Durable reminders: enqueue into the outbox and run the delivery worker.

mod cursor;
mod delivery;
mod enqueue;

pub use cursor::OutboxScheduleCursor;
pub use delivery::{OutboxDelivery, TelegramReminderSender};
pub use enqueue::{
    MechanicalReminder, enqueue_mechanical, ensure_schedule_cursor, record_schedule_fire,
    spawn_worker,
};
