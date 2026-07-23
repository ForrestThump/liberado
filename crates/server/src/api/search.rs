//! Conversation history search endpoint.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::Deserialize;

use chat_client_contract::{
    ApiError, ConversationSearchResponse, ConversationSearchResult, SearchMessageMatch,
};
use liberado_chat_search::ParsedQuery;

use crate::state::AppState;
#[derive(Deserialize)]
pub struct SearchQuery {
    pub q: String,
    #[serde(default)]
    pub regex: bool,
    #[serde(default = "default_search_limit")]
    pub limit: usize,
}

fn default_search_limit() -> usize {
    20
}

/// `GET /api/conversations/search?q=...&regex=false&limit=20`
///
/// Searches conversation history for messages matching `q`. In literal mode (default), `q` is
/// split on whitespace; `"quoted phrases"` are treated as single terms; ALL terms must appear in
/// **the same message** (case-insensitive AND) â€” narrows toward a topic from a few
/// half-remembered keywords rather than flooding results with an OR. This is per-message, not
/// per-conversation: a query like `"auth token"` will not match a conversation where "auth" and
/// "token" appear in two different messages, only one where a single message contains both. In
/// regex mode, `q` is a single Rust regex pattern applied case-insensitively (also per-message).
///
/// Returns at most `limit` matching conversations (newest first), each with every matching
/// message's snippet. 400 on an empty query or invalid regex; 500 on I/O error.
pub async fn search_conversations(
    State(state): State<Arc<AppState>>,
    Query(query): Query<SearchQuery>,
) -> impl IntoResponse {
    let parsed = if query.regex {
        ParsedQuery::parse_regex(&query.q)
    } else {
        ParsedQuery::parse_literal(&query.q)
    };
    let parsed = match parsed {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiError {
                    error: e.to_string(),
                }),
            )
                .into_response();
        }
    };

    let limit = query.limit.clamp(1, 200);
    match liberado_chat_search::search(&state.sessions_root, &parsed, limit).await {
        Ok(sr) => {
            let total_found = sr.total_found;
            let results = sr
                .matches
                .into_iter()
                .map(|m| ConversationSearchResult {
                    conversation_id: m.conversation_id,
                    title: m.title,
                    created_at: m.created_at,
                    matches: m
                        .matches
                        .into_iter()
                        .map(|mm| SearchMessageMatch {
                            node_id: mm.node_id,
                            author: mm.author,
                            content_snippet: mm.content_snippet,
                            created_at: mm.created_at,
                        })
                        .collect(),
                })
                .collect();
            Json(ConversationSearchResponse {
                results,
                total_found,
            })
            .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}
