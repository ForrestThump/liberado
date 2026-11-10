//! HTTP/SSE surface handlers, partitioned by route group (see `docs/spec/reference/api.md`).

mod approvals;
mod auth;
mod automations;
mod chat;
mod chat_create;
mod completions;
#[cfg(test)]
mod completions_http_tests;
mod goals;
mod grants;
mod library;
mod mcp_status;
mod outbox_action;
#[cfg(test)]
mod outbox_action_http_tests;
mod outbox_apply;
mod outbox_detail;
#[cfg(test)]
mod outbox_detail_http_tests;
mod outbox_log;
mod plugins;
mod profiles;
mod push;
#[cfg(test)]
mod recurring_done_http_tests;
mod reminders;
#[cfg(test)]
mod reminders_http_tests;
pub(crate) mod schedule_description;
mod search;
mod sessions;
mod status;
mod vault_task_line;

pub use approvals::{list_approvals, list_pending_approvals, resolve_approval};
pub use auth::{
    delete_auth_session, delete_passkey, health, list_auth_sessions, list_passkeys,
    login as auth_login, logout as auth_logout, patch_passkey, session as auth_session,
};
pub use automations::{list_hooks, list_schedules};
pub use chat::{
    attach_conversation, cancel_conversation_turn, chat, chat_stream_get, chat_stream_post,
    delete_conversation, get_conversation, list_conversations, list_profiles,
    patch_conversation_title, set_conversation_profile,
};
pub use chat_create::create_conversation;
pub use completions::list_completions;
pub use goals::{
    goals_cancel, goals_diff, goals_domains, goals_get, goals_list, goals_message, goals_park,
    goals_rewind, goals_start, goals_stream, list_projects,
};
pub use grants::{conversation_grants, get_header_grant};
pub use library::list_library;
pub use outbox_action::outbox_action_with_state;
pub use outbox_detail::{complete_outbox, get_outbox_detail, snooze_outbox};
pub use plugins::set_plugin_enabled;
pub use profiles::{delete_profile, rename_profile};
pub use push::push_weekly;
pub use reminders::{
    delete_subscription, list_outbox, list_subscriptions, outbox_ack_with_state, push_status,
    push_test, report_client_error, retry_outbox, upsert_subscription, vapid_public_key,
};
pub use search::search_conversations;
pub use sessions::{session_fork, sessions_list};
pub use status::{catalog, models, reactions, reload_mcp_peers, select_model, status, vault};

#[cfg(test)]
#[path = "auth_router_http_tests.rs"]
mod auth_router_http_tests;

#[cfg(test)]
#[path = "auth_management_http_tests.rs"]
mod auth_management_http_tests;
