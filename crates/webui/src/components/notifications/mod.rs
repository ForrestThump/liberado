//! Weekly spending notifications on the Automations page.
//!
//! The permission request runs in the click handler, before any await.
//! iOS only grants it for an installed home-screen app (iOS 16.4+).

mod actions;
mod browser;
mod card;
mod model;

pub use card::NotificationsCard;
