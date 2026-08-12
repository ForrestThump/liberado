//! HTTP/SSE surface handlers, partitioned by route group (see `docs/spec/reference/api.md`).

mod chat;
mod goals;
mod search;
mod sessions;
mod status;

pub use chat::{
    attach_conversation, cancel_conversation_turn, chat, chat_stream_get, chat_stream_post,
    delete_conversation, get_conversation, list_conversations, list_profiles,
    patch_conversation_title, set_conversation_profile,
};
pub use goals::{
    goals_cancel, goals_diff, goals_domains, goals_get, goals_list, goals_message, goals_park,
    goals_rewind, goals_start, goals_stream, list_projects,
};
pub use search::search_conversations;
pub use sessions::{session_fork, sessions_list};
pub use status::{catalog, models, reactions, reload_mcp_peers, select_model, status, vault};
