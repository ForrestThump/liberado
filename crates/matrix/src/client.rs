//! Homeserver client. Every call sends `Authorization: Bearer <as_token>` and
//! `user_id` of the acting user. The acting user for the main room is `@liberado`.
//!
//! Tuwunel allows that masquerade when the user id matches the application-service
//! namespace. It does not require the user to have been created by the application service.
//! The existing `@liberado` account can be driven this way.

use std::time::Duration;

use liberado_messaging::MessagingError;
use reqwest::Method;
use serde_json::{Value, json};

use crate::encode::encode_segment;
use crate::room::direct_room_body;

const TIMEOUT: Duration = Duration::from_secs(15);

/// Client-Server API calls used by the main room.
#[derive(Clone)]
pub struct MatrixClient {
    http: reqwest::Client,
    homeserver: String,
    as_token: String,
    user_id: String,
}

impl MatrixClient {
    /// `homeserver` is the base URL, for example `http://tuwunel:8008`.
    pub fn new(homeserver: &str, as_token: &str, user_id: &str) -> Result<Self, String> {
        let http = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .connect_timeout(Duration::from_secs(5))
            .build()
            .map_err(|error| format!("matrix http client: {error}"))?;
        Ok(Self {
            http,
            homeserver: homeserver.trim_end_matches('/').to_string(),
            as_token: as_token.to_string(),
            user_id: user_id.to_string(),
        })
    }

    pub fn user_id(&self) -> &str {
        &self.user_id
    }

    /// `GET /_matrix/client/v3/account/whoami`.
    pub async fn whoami(&self) -> Result<String, MessagingError> {
        let body = self.get(&["account", "whoami"]).await?;
        Ok(string_field(&body, "user_id"))
    }

    /// Room id for an alias (`#liberado:server`).
    pub async fn resolve_alias(&self, alias: &str) -> Result<String, MessagingError> {
        let body = self
            .get(&["directory", "room", alias])
            .await?
            .get("room_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if body.is_empty() {
            return Err(MessagingError::new("matrix alias response had no room_id"));
        }
        Ok(body)
    }

    /// User ids currently in the room.
    pub async fn joined_members(&self, room_id: &str) -> Result<Vec<String>, MessagingError> {
        let body = self.get(&["rooms", room_id, "joined_members"]).await?;
        Ok(member_ids(&body))
    }

    /// `POST /_matrix/client/v3/join/{roomIdOrAlias}`.
    pub async fn join_room(&self, room_id_or_alias: &str) -> Result<(), MessagingError> {
        self.post(&["join", room_id_or_alias], &json!({})).await?;
        Ok(())
    }

    /// Send `content` as `m.room.message`. Returns the new event id.
    pub async fn send_message(
        &self,
        room_id: &str,
        content: &Value,
    ) -> Result<String, MessagingError> {
        let txn = ulid::Ulid::new().to_string();
        let body = self
            .put(&["rooms", room_id, "send", "m.room.message", &txn], content)
            .await?;
        event_id(&body)
    }

    /// `PUT /_matrix/client/v3/rooms/{room}/typing/{user}`.
    pub async fn set_typing(&self, room_id: &str, typing: bool) -> Result<(), MessagingError> {
        let user = self.user_id.clone();
        let body = json!({ "typing": typing, "timeout": 20_000 });
        self.put(&["rooms", room_id, "typing", &user], &body)
            .await?;
        Ok(())
    }

    /// `PUT /_matrix/client/v3/rooms/{room}/state/{type}/{stateKey}`.
    pub async fn put_state(
        &self,
        room_id: &str,
        event_type: &str,
        state_key: &str,
        content: &Value,
    ) -> Result<(), MessagingError> {
        if state_key.is_empty() {
            self.put(&["rooms", room_id, "state", event_type], content)
                .await?;
        } else {
            self.put(&["rooms", room_id, "state", event_type, state_key], content)
                .await?;
        }
        Ok(())
    }

    /// Create a direct room with encryption locked off. Returns the room id.
    pub async fn create_direct_room(
        &self,
        name: &str,
        invite_mxid: &str,
        owner_mxid: &str,
    ) -> Result<String, MessagingError> {
        let body = direct_room_body(name, invite_mxid, owner_mxid, &self.user_id);
        let response = self.post(&["createRoom"], &body).await?;
        let room_id = string_field(&response, "room_id");
        if room_id.is_empty() {
            return Err(MessagingError::new(
                "matrix createRoom response had no room_id",
            ));
        }
        Ok(room_id)
    }

    /// `POST /_matrix/client/v1/appservice/{id}/ping`.
    pub async fn ping_appservice(&self, as_id: &str) -> Result<(), MessagingError> {
        let url = self.url_v1(&["appservice", as_id, "ping"]);
        self.exchange(Method::POST, &url, Some(&json!({}))).await?;
        Ok(())
    }

    async fn get(&self, segments: &[&str]) -> Result<Value, MessagingError> {
        self.exchange(Method::GET, &self.url(segments), None).await
    }

    async fn post(&self, segments: &[&str], body: &Value) -> Result<Value, MessagingError> {
        self.exchange(Method::POST, &self.url(segments), Some(body))
            .await
    }

    async fn put(&self, segments: &[&str], body: &Value) -> Result<Value, MessagingError> {
        self.exchange(Method::PUT, &self.url(segments), Some(body))
            .await
    }

    fn url(&self, segments: &[&str]) -> String {
        self.with_user(&self.endpoint(&client_prefix(segments)))
    }

    fn url_v1(&self, segments: &[&str]) -> String {
        let mut parts = vec!["_matrix", "client", "v1"];
        parts.extend_from_slice(segments);
        self.with_user(&self.endpoint(&parts))
    }

    fn endpoint(&self, segments: &[&str]) -> String {
        let mut url = self.homeserver.clone();
        for segment in segments {
            url.push('/');
            url.push_str(&encode_segment(segment));
        }
        url
    }

    fn with_user(&self, url: &str) -> String {
        format!("{url}?user_id={}", encode_segment(&self.user_id))
    }

    async fn exchange(
        &self,
        method: Method,
        url: &str,
        body: Option<&Value>,
    ) -> Result<Value, MessagingError> {
        let mut request = self.http.request(method, url).bearer_auth(&self.as_token);
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request
            .send()
            .await
            .map_err(|error| MessagingError::new(self.redact(&error.to_string())))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|error| MessagingError::new(self.redact(&error.to_string())))?;
        if !status.is_success() {
            return Err(MessagingError::new(self.failure(status.as_u16(), &text)));
        }
        parse_body(&text, status.as_u16())
    }

    fn failure(&self, status: u16, body: &str) -> String {
        let redacted = self.redact(body);
        let errcode = errcode_of(&redacted);
        match errcode {
            Some(code) => format!("matrix http {status} {code}"),
            None => format!("matrix http {status}"),
        }
    }

    fn redact(&self, text: &str) -> String {
        let cleaned = if self.as_token.is_empty() {
            text.to_string()
        } else {
            text.replace(&self.as_token, "[redacted]")
        };
        cleaned.chars().take(240).collect()
    }
}

fn client_prefix<'a>(segments: &'a [&str]) -> Vec<&'a str> {
    let mut parts = vec!["_matrix", "client", "v3"];
    parts.extend_from_slice(segments);
    parts
}

fn parse_body(text: &str, status: u16) -> Result<Value, MessagingError> {
    if text.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(text)
        .map_err(|_| MessagingError::new(format!("matrix response was not json ({status})")))
}

fn errcode_of(body: &str) -> Option<String> {
    let value: Value = serde_json::from_str(body).ok()?;
    value.get("errcode")?.as_str().map(str::to_string)
}

fn string_field(body: &Value, field: &str) -> String {
    body.get(field)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn event_id(body: &Value) -> Result<String, MessagingError> {
    let id = string_field(body, "event_id");
    if id.is_empty() {
        return Err(MessagingError::new("matrix send response had no event_id"));
    }
    Ok(id)
}

fn member_ids(body: &Value) -> Vec<String> {
    body.get("joined")
        .and_then(Value::as_object)
        .map(|joined| joined.keys().cloned().collect())
        .unwrap_or_default()
}

/// Case-insensitive membership test.
pub fn member_present(members: &[String], user_id: &str) -> bool {
    members.iter().any(|id| id.eq_ignore_ascii_case(user_id))
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;
