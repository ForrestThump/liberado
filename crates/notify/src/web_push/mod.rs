//! Web Push (RFC 8291 + RFC 8292) for reminders.
//!
//! The private VAPID key comes from the environment. This module never logs it.
//! The crypto is ported from `origin/feat/weekly-spending-push` @ `ac9a7fa4` (PR #317).
//! Weekly spending and its per-subscription week stamp stayed on that branch.

mod aesgcm;
mod b64;
mod send;
mod vapid;

pub use send::{PushDelivery, PushRequest, REMINDER_TTL, post_push, reminder_payload};
pub use vapid::vapid_env_lines;

use vapid::VapidMaterial;

/// Loaded VAPID material, or `None` when the three variables are unset.
pub fn load_vapid() -> Result<Option<VapidMaterial>, String> {
    vapid::load_optional()
}

pub use vapid::VapidMaterial as VapidKeys;
