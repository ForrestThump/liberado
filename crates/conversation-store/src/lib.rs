//! # liberado-conversation-store
//!
//! The Decision-17 conversation-history store. There is exactly one thing it must not lose: an
//! **append-only log of message nodes**. That log is a **DAG** (`id` + `parent_id`) — a linear
//! chat is the degenerate case where every node's parent is the previous leaf — so branching,
//! fork, loop-back and debate become *additive* later rather than a migration.
//!
//! It lives as **JSONL outside the vault**: conversation history is operational data (the Decision
//! 12 category), and high-volume chat writes must not pollute the vault change-stream the daemon
//! reacts to. The vault stays the source of truth for *knowledge*; an export to Markdown is a
//! derived view, never the system of record.
//!
//! The one non-negotiable property is that node ids are **time-sortable and minted at append time
//! by a single writer**, so the log is intrinsically sorted by id (file order == id order). That
//! is what makes random lookup an index-free binary search and makes a parent always earlier in the
//! log. Appends to a conversation are serialized (single-writer); reads are lock-free.
//!
//! Everything else — the line-offset index, Markdown/vector projections, a SQLite or Postgres
//! engine — is a derived projection or a swap-in behind the [`ConversationStore`] trait, with no
//! schema change. See `liberado-conversation-store-spec.md` for the full rationale.
//!
//! v1 ships the schema, the trait, and one [`JsonlStore`] implementation.

mod error;
mod jsonl;
mod store;
mod types;

pub use error::{StoreError, StoreResult};
pub use jsonl::JsonlStore;
pub use store::ConversationStore;
pub use types::{Author, ConversationHeader, MessageNode, NewConversation, NewNode, Timestamp};
// Re-exported so downstream crates can name session/node ids without depending on `ulid` directly.
pub use ulid::Ulid;
