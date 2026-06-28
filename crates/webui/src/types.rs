//! Wire DTO re-exports for the WebUI.
//!
//! All types are sourced from `chat-client-contract` (the single source of truth).
//! This module exists solely for backward-compatible import paths (`crate::types::Foo`)
//! inside the WebUI; no new definitions live here.

pub use chat_client_contract::{ApiError, DaemonStatus, ReactionEvent, ReactionOutcome, VaultInfo};
