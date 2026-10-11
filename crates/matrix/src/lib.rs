//! Matrix application service for the main Liberado room.
//!
//! The homeserver is Tuwunel. One application service drives `@liberado` by masquerade:
//! every Client-Server call sends the application-service token and `user_id=@liberado:…`.
//! Tuwunel allows that for any user id inside the registration namespace, including an
//! account that was created earlier with a password. Later rooms and agent users are not
//! started here.
//!
//! Matrix stays off unless `LIBERADO_MATRIX_HOMESERVER`, `LIBERADO_MATRIX_AS_TOKEN`,
//! `LIBERADO_MATRIX_HS_TOKEN`, `LIBERADO_MATRIX_OWNER`, and `LIBERADO_MATRIX_MAIN_ROOM`
//! are all set.

mod client;
mod config;
mod encode;
mod filter;
mod hub;
mod listener;
mod messages;
mod room;
mod token;
mod txn_log;

pub use client::MatrixClient;
pub use config::{Activation, EnvSnapshot, MatrixSettings, activation, missing_names};
pub use hub::RoomQueues;
pub use room::{direct_room_body, encryption_is_locked, encryption_locked_power_levels};
