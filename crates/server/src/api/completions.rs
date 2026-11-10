//! Read the task-completion log. Owner only. Nothing here writes a row.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::state::AppState;

use super::outbox_apply::fail;

const DEFAULT_LIMIT: u32 = 100;
const MAX_LIMIT: u32 = 500;

#[derive(Deserialize)]
pub struct CompletionsQuery {
    since: Option<String>,
    limit: Option<u32>,
}

pub async fn list_completions(
    State(state): State<Arc<AppState>>,
    Query(query): Query<CompletionsQuery>,
) -> Response {
    let Some(outbox) = state.outbox.as_ref() else {
        return fail(StatusCode::SERVICE_UNAVAILABLE, "outbox is not available");
    };
    let since = match since_stamp(query.since.as_deref()) {
        Ok(since) => since,
        Err(message) => return fail(StatusCode::BAD_REQUEST, message),
    };
    let limit = match checked_limit(query.limit) {
        Ok(limit) => limit,
        Err(message) => return fail(StatusCode::BAD_REQUEST, message),
    };
    match outbox.list_completions(since.as_deref(), limit) {
        Ok(rows) => Json(rows).into_response(),
        Err(err) => fail(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string()),
    }
}

fn since_stamp(since: Option<&str>) -> Result<Option<String>, &'static str> {
    let Some(text) = since.map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(None);
    };
    let parsed = DateTime::parse_from_rfc3339(text).map_err(|_| "since must be RFC3339")?;
    Ok(Some(liberado_outbox::completion_stamp(
        parsed.with_timezone(&Utc),
    )))
}

fn checked_limit(limit: Option<u32>) -> Result<u32, &'static str> {
    let Some(limit) = limit else {
        return Ok(DEFAULT_LIMIT);
    };
    if (1..=MAX_LIMIT).contains(&limit) {
        Ok(limit)
    } else {
        Err("limit must be from 1 to 500")
    }
}
