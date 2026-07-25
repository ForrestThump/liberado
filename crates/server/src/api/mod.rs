//! HTTP/SSE surface handlers, partitioned by route group (see `docs/reference/api.md`).

mod chat;
mod goals;
mod search;
mod sessions;
mod status;

pub use chat::{
    chat, chat_stream_get, chat_stream_post, get_conversation, list_conversations,
    patch_conversation_title,
};
pub use goals::{
    goals_cancel, goals_domains, goals_get, goals_list, goals_message, goals_park, goals_start,
    goals_stream,
};
pub use search::search_conversations;
pub use sessions::{session_fork, sessions_list};
pub use status::{catalog, models, reactions, reload_mcp_peers, select_model, status, vault};
