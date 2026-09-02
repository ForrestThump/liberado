//! The converged `Session` store (session-focus S5′, decision D7).
//!
//! Liberado used to have two stores that did the same job:
//!
//! * `liberado-conversation-store` — chats. A DAG of message nodes.
//! * `liberado-session::GoalSessionStore` — goal sessions. A flat event log.
//!
//! D7 says these were never two things: a conversation and a goal session are one **`Session`**, and
//! the differences are *attributes, not subtypes* — `goal: Option`, `origin`, `visibility`, `grant`.
//! This crate is where that becomes true on disk. One directory, one id space, one log per session
//! holding both message nodes and pack events.
//!
//! # Two lenses, one engine
//!
//! [`SessionStore`] implements **both** [`ConversationStore`](liberado_conversation_store::ConversationStore)
//! (nodes + messages) and [`SessionRecordStore`](liberado_session::SessionRecordStore) (records +
//! events). That is not a compromise, it is a layer rule doing its job: the session kernel is
//! forbidden from depending on `liberado-provider`, so it cannot know what a `Message` is — and a
//! store that must hold both has to live *above* the kernel and expose a kernel-shaped view
//! downward. The duplication S5′ removed was in the **storage**; what remains is two typed views of
//! one log, which is exactly what the layering asks for.
//!
//! # What convergence buys
//!
//! * `origin` is a **real edge** (`parent_session: Ulid`), not a stringly-typed cross-store
//!   reference — the kernel previously had to name the parent conversation by `String` because it
//!   was not allowed to depend on the store that held it.
//! * One `list_sessions()` — the unified switcher no longer polls two endpoints and glues them.
//! * A cron/hook/subagent run can be a **background session**, so it stops firing into the void.
//! * Goal sessions inherit the message DAG, which is what makes branching and forking additive
//!   rather than a migration.

mod jsonl;
mod types;

pub use jsonl::{Record, SessionStore};
pub use types::{NewSession, SessionHeader};

/// Re-exported from the kernel, where it now lives: `visibility` is an attribute of a *session*, and
/// the kernel's `GoalSessionRecord` is one of the two lenses onto a session — so while this enum
/// lived only up here, that lens could not carry it, and the store had no choice but to stamp
/// everything it received `Foreground`. That is exactly why nothing could emit a background session
/// until S5′ step 5.
pub use liberado_session::Visibility;

pub use ulid::Ulid;

#[cfg(test)]
mod leaf_path_tests;
#[cfg(test)]
mod tests;
