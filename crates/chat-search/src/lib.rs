//! Full-text and regex search over the Liberado conversation JSONL store.
//!
//! One shared implementation consumed by two front-ends: `liberado-server`'s
//! `GET /api/conversations/search` (for the webui) and `liberado-chat-search-mcp` (so the
//! dispatcher can search chat history mid-reasoning). The store's own per-conversation JSONL
//! layout (`liberado_conversation_store`) was already designed to "stay greppable" — this crate
//! is that design intent realized.
//!
//! Entry point: [`search`].

mod query;
mod scan;

pub use query::{ParsedQuery, QueryParseError};
pub use scan::{ConversationMatch, MessageMatch, SearchError, SearchResults, search};
