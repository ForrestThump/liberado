//! HTTP/SSE surface handlers, partitioned by route group (see `docs/spec/reference/api.md`).

mod approvals;
mod auth;
mod automations;
mod chat;
mod chat_create;
mod goals;
mod grants;
mod library;
mod plugins;
mod profiles;
pub(crate) mod schedule_description;
mod search;
mod sessions;
mod status;

pub use approvals::{list_approvals, list_pending_approvals, resolve_approval};
pub use auth::{health, login as auth_login, logout as auth_logout, session as auth_session};
pub use automations::{list_hooks, list_schedules};
pub use chat::{
    attach_conversation, cancel_conversation_turn, chat, chat_stream_get, chat_stream_post,
    delete_conversation, get_conversation, list_conversations, list_profiles,
    patch_conversation_title, set_conversation_profile,
};
pub use chat_create::create_conversation;
pub use goals::{
    goals_cancel, goals_diff, goals_domains, goals_get, goals_list, goals_message, goals_park,
    goals_rewind, goals_start, goals_stream, list_projects,
};
pub use grants::{conversation_grants, get_header_grant};
pub use library::list_library;
pub use plugins::set_plugin_enabled;
pub use profiles::{delete_profile, rename_profile};
pub use search::search_conversations;
pub use sessions::{session_fork, sessions_list};
pub use status::{catalog, models, reactions, reload_mcp_peers, select_model, status, vault};

#[cfg(test)]
#[path = "auth_router_http_tests.rs"]
mod auth_router_http_tests;
