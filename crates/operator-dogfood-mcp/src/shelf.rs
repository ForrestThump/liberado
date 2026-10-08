//! Pure matching for Agents-shelf identity and history rows.
//!
//! The rules match `ChatSessions::create_agent_chat`: one agent per trimmed
//! `(profile, title)`, a blank title uses the profile name, the Agent Creator
//! singleton is not a match, and the oldest row wins.
//!
//! Oldest matches the daemon's `pick_oldest` (`crates/main-agent/src/sessions/agent_spawn.rs`):
//! parsed RFC3339 `created_at` as `DateTime<Utc>`, then `id`. `GET /api/conversations`
//! serializes `ConversationHeader.created_at` with chrono `SecondsFormat::AutoSi`
//! (0, 3, 6, or 9 fractional digits), so the raw wire strings are not chronological:
//! `2026-02-01T00:00:00.100001Z` sorts before `2026-02-01T00:00:00.100Z`.
//!
//! A missing or unparseable `created_at` is treated as `DateTime::<Utc>::MAX_UTC`,
//! so it never wins against a parseable timestamp. Among those rows, `id` still
//! breaks the tie. Issued ids are uppercase 26-character ULIDs; Crockford base32
//! is ASCII-monotonic, so wire-string `id` order equals `Ulid` Ord.

use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use crate::error::DogfoodError;

/// Tool names from `liberado-agent-workspace`. This crate does not depend on
/// that kernel crate; the smoke check only looks for these strings.
pub const WORKSPACE_TOOL_NAMES: [&str; 5] = [
    "workspace_list",
    "workspace_read",
    "workspace_write",
    "workspace_delete",
    "workspace_download",
];

const WORKSPACE_NOTE: &str = "Read from GET /api/status chat_tool_names. The boot catalog lists \
     workspace tools only when the workspace root is usable. This check does not write files and \
     does not call a workspace route.";

/// `(profile, title)` identity. Empty profile is refused. Blank title uses the
/// profile name. Comparison is case-sensitive, after trim.
pub fn agent_identity(
    profile: &str,
    title: Option<&str>,
) -> Result<(String, String), DogfoodError> {
    let profile = profile.trim();
    if profile.is_empty() {
        return Err(DogfoodError::Invalid(
            "create_agent requires a non-empty profile".into(),
        ));
    }
    let title = blank_to_none(title).unwrap_or(profile).to_owned();
    Ok((profile.to_owned(), title))
}

/// Trim to `None` when absent or blank.
pub fn blank_to_none(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|text| !text.is_empty())
}

/// Session id safe to place in one path segment.
///
/// Daemon conversation ids are 26-character Crockford-base32 ULIDs. The store
/// mints them with `ulid::Generator`; `GET /api/conversations/{id}` extracts
/// `Path<Ulid>` and `POST /api/chat` takes `session: Option<Ulid>`. Requiring
/// that alphabet rejects `.`, `..`, and percent-encoded dot segments (`%2e%2e`)
/// that reqwest's URL parser would otherwise normalize out of
/// `/api/conversations/{id}` onto `/api/` or `/api/conversations/`.
pub fn session_id(raw: &str) -> Result<&str, DogfoodError> {
    let id = raw.trim();
    if id.is_empty() {
        return Err(DogfoodError::Invalid("session id is required".into()));
    }
    if is_conversation_ulid(id) {
        Ok(id)
    } else {
        Err(DogfoodError::Invalid(
            "session id must be the 26-character conversation ULID".into(),
        ))
    }
}

/// The human line the caller typed. Blank is refused so a tool cannot post an
/// empty turn in place of a missing human reply.
pub fn human_message(raw: &str) -> Result<&str, DogfoodError> {
    let message = raw.trim();
    if message.is_empty() {
        return Err(DogfoodError::Invalid(
            "message is required. This tool does not invent a human reply.".into(),
        ));
    }
    Ok(message)
}

/// `GET /api/profiles`: `true` only when that name is enabled and
/// `agent_eligible` is JSON true. A missing flag is not eligible.
pub fn profile_eligibility(body: &Value, name: &str) -> Result<bool, DogfoodError> {
    let profiles = body
        .get("profiles")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            DogfoodError::Daemon("GET /api/profiles returned no profiles array".into())
        })?;
    let Some(row) = profiles
        .iter()
        .find(|row| row.get("name").and_then(Value::as_str) == Some(name))
    else {
        return Err(DogfoodError::Invalid(format!(
            "profile `{name}` is not an enabled session profile"
        )));
    };
    Ok(row.get("agent_eligible").and_then(Value::as_bool) == Some(true))
}

/// Oldest Agents-shelf row with this profile and title, skipping the creator.
pub fn oldest_match<'a>(headers: &'a [Value], profile: &str, title: &str) -> Option<&'a Value> {
    headers
        .iter()
        .filter(|header| is_named_agent(header, profile, title))
        .min_by(|left, right| row_key(left).cmp(&row_key(right)))
}

/// Any Agents-shelf row except the Agent Creator singleton.
pub fn is_shelf_agent(header: &Value) -> bool {
    !flag_true(header, "agent_creator") && surface_is_agent(header)
}

/// Wire row for one shelf agent. Keeps the list order the daemon sent.
pub fn slim_agent(header: &Value) -> Value {
    json!({
        "id": header.get("id").cloned().unwrap_or(Value::Null),
        "title": header.get("title").cloned().unwrap_or(Value::Null),
        "profile": grant_profile(header),
        "created_at": header.get("created_at").cloned().unwrap_or(Value::Null),
        "surface_mode": "agent",
    })
}

/// Short row for `GET /api/sessions`.
pub fn slim_session(header: &Value) -> Value {
    json!({
        "id": header.get("id").cloned().unwrap_or(Value::Null),
        "title": header.get("title").cloned().unwrap_or(Value::Null),
        "surface_mode": header.get("surface_mode").cloned().unwrap_or(json!("chat")),
        "profile": grant_profile(header),
        "status": header.get("status").cloned().unwrap_or(Value::Null),
        "has_goal": header.get("goal").is_some_and(|goal| !goal.is_null()),
        "agent_creator": flag_true(header, "agent_creator"),
    })
}

/// Content of the last assistant message, if the transcript has one.
pub fn latest_assistant(history: &Value) -> Option<Value> {
    let messages = history.get("messages")?.as_array()?;
    messages
        .iter()
        .rev()
        .find(|message| message.get("role").and_then(Value::as_str) == Some("assistant"))
        .and_then(|message| message.get("content"))
        .cloned()
}

/// Which workspace tool names the status catalog listed.
pub fn workspace_report(names: &[String]) -> Value {
    let mut present = Vec::new();
    let mut missing = Vec::new();
    for bare in WORKSPACE_TOOL_NAMES {
        if names.iter().any(|name| tool_is(name, bare)) {
            present.push(bare);
        } else {
            missing.push(bare);
        }
    }
    json!({
        "present": present,
        "missing": missing,
        "read_only": true,
        "note": WORKSPACE_NOTE,
    })
}

/// String entries of a JSON array. A missing or non-array field is empty.
pub fn string_list(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

pub fn header_id(header: &Value) -> Result<&str, DogfoodError> {
    header
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| DogfoodError::Daemon("conversation header had no id".into()))
}

const ULID_LEN: usize = 26;

fn is_conversation_ulid(id: &str) -> bool {
    id.len() == ULID_LEN && id.bytes().all(is_crockford_base32)
}

fn is_crockford_base32(b: u8) -> bool {
    b"0123456789ABCDEFGHJKMNPQRSTVWXYZ".contains(&b.to_ascii_uppercase())
}

fn is_named_agent(header: &Value, profile: &str, title: &str) -> bool {
    !flag_true(header, "agent_creator")
        && surface_is_agent(header)
        && grant_profile_str(header) == Some(profile)
        && header.get("title").and_then(Value::as_str) == Some(title)
}

fn surface_is_agent(header: &Value) -> bool {
    header.get("surface_mode").and_then(Value::as_str) == Some("agent")
}

fn flag_true(header: &Value, name: &str) -> bool {
    header.get(name).and_then(Value::as_bool) == Some(true)
}

fn grant_profile(header: &Value) -> Value {
    match grant_profile_str(header) {
        Some(name) => Value::String(name.to_owned()),
        None => Value::Null,
    }
}

fn grant_profile_str(header: &Value) -> Option<&str> {
    header
        .get("grant")
        .and_then(|grant| grant.get("profile"))
        .and_then(Value::as_str)
}

fn row_key(header: &Value) -> (DateTime<Utc>, String) {
    (
        parsed_created_at(header).unwrap_or(DateTime::<Utc>::MAX_UTC),
        text_field(header, "id"),
    )
}

fn parsed_created_at(header: &Value) -> Option<DateTime<Utc>> {
    let raw = header.get("created_at").and_then(Value::as_str)?;
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|ts| ts.with_timezone(&Utc))
}

fn text_field(header: &Value, name: &str) -> String {
    header
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}

fn tool_is(name: &str, bare: &str) -> bool {
    name == bare
        || name
            .strip_suffix(bare)
            .is_some_and(|prefix| prefix.ends_with(':'))
}

#[cfg(test)]
#[path = "shelf_tests.rs"]
mod tests;
