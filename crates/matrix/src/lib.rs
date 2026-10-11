//! Matrix application service for the main Liberado room and one direct chat per agent.
//!
//! The homeserver is Tuwunel. One application service drives `@liberado` by masquerade:
//! every Client-Server call sends the application-service token and `user_id=@liberado:…`.
//! Tuwunel allows that for any user id inside the registration namespace, including an
//! account that was created earlier with a password. Agent puppets are
//! `@liberado_<slug>:<server>` in that same namespace. The main room is not created here.
//!
//! Matrix stays off unless `LIBERADO_MATRIX_HOMESERVER`, `LIBERADO_MATRIX_AS_TOKEN`,
//! `LIBERADO_MATRIX_HS_TOKEN`, `LIBERADO_MATRIX_OWNER`, and `LIBERADO_MATRIX_MAIN_ROOM`
//! are all set.

mod agents;
mod cards;
mod channel;
mod client;
mod commands;
mod config;
mod encode;
mod filter;
mod hub;
mod listener;
mod messages;
mod puppet;
mod room;
mod slug;
mod startup;
mod token;
mod txn_log;

pub use agents::{AgentDirectory, AgentRecord, AgentSeat, ReconcileReport, reconcile_agents};
pub use channel::{MatrixCardUpdater, MatrixChannel};
pub use client::MatrixClient;
pub use config::{Activation, EnvSnapshot, MatrixSettings, activation, missing_names};
pub use hub::RoomQueues;
pub use puppet::user_in_use;
pub use room::{direct_room_body, encryption_is_locked, encryption_locked_power_levels};
pub use slug::slug_of_mxid;
pub use startup::{RunningMatrix, connect, prepare_room};
