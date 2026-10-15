//! Which inbound events the main room acts on.
//!
//! The owner is the only human sender. Echoes from the Liberado namespace, the main user,
//! and the application-service sender are ignored. An encrypted event is answered once
//! by the listener, not turned into a chat message.

use serde_json::Value;

/// What to do with one event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// Sent by a user we drive.
    Echo,
    /// Sent by someone other than the owner.
    Stranger,
    /// Owner sent `m.room.encrypted`.
    Encrypted,
    /// Owner sent an `m.room.message` with text.
    Message,
    /// Anything else.
    Other,
}

/// The ids the filter compares against.
#[derive(Debug, Clone)]
pub struct AcceptPolicy {
    pub owner: String,
    pub server_name: String,
    pub main_user: String,
    pub sender_localpart: String,
}

/// Fields the listener needs from one transaction event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawEvent {
    pub room_id: String,
    pub sender: String,
    pub event_type: String,
    pub body: String,
}

/// Parse one event object. Missing room, sender, or type drops the event.
pub fn parse_event(value: &Value) -> Option<RawEvent> {
    Some(RawEvent {
        room_id: value.get("room_id")?.as_str()?.to_string(),
        sender: value.get("sender")?.as_str()?.to_string(),
        event_type: value.get("type")?.as_str()?.to_string(),
        body: value
            .get("content")
            .and_then(|content| content.get("body"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    })
}

/// Classify `sender` and `event_type`. Message text is checked by the caller.
pub fn classify(sender: &str, event_type: &str, policy: &AcceptPolicy) -> Class {
    if is_ours(sender, policy) {
        return Class::Echo;
    }
    if !sender.eq_ignore_ascii_case(&policy.owner) {
        return Class::Stranger;
    }
    match event_type {
        "m.room.encrypted" => Class::Encrypted,
        "m.room.message" => Class::Message,
        _ => Class::Other,
    }
}

/// True for the main user, the sender localpart, or anyone in the Liberado namespace.
pub fn is_ours(sender: &str, policy: &AcceptPolicy) -> bool {
    sender.eq_ignore_ascii_case(&policy.main_user)
        || sender.eq_ignore_ascii_case(&bridge_mxid(policy))
        || in_user_namespace(sender, &policy.server_name)
}

fn bridge_mxid(policy: &AcceptPolicy) -> String {
    format!("@{}:{}", policy.sender_localpart, policy.server_name)
}

/// `@liberado:server` and `@liberado_<suffix>:server`, case-insensitive.
///
/// The whole user id must match, the same shape as the anchored registration regex
/// `^@liberado(_[a-z0-9._=-]+)?:<server>$`. A longer string that only contains that id
/// does not match. The suffix is `_[a-z0-9._=-]+`.
pub fn in_user_namespace(mxid: &str, server_name: &str) -> bool {
    let Some((localpart, host)) = split_mxid(mxid) else {
        return false;
    };
    if !host.eq_ignore_ascii_case(server_name) {
        return false;
    }
    namespace_localpart(&localpart.to_ascii_lowercase())
}

fn split_mxid(mxid: &str) -> Option<(&str, &str)> {
    let rest = mxid.strip_prefix('@')?;
    let (localpart, host) = rest.rsplit_once(':')?;
    (!localpart.is_empty() && !host.is_empty()).then_some((localpart, host))
}

fn namespace_localpart(localpart: &str) -> bool {
    let Some(suffix) = localpart.strip_prefix("liberado") else {
        return false;
    };
    suffix.is_empty() || valid_agent_suffix(suffix)
}

fn valid_agent_suffix(suffix: &str) -> bool {
    let Some(body) = suffix.strip_prefix('_') else {
        return false;
    };
    !body.is_empty()
        && body
            .bytes()
            .all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'=' | b'-'))
}

/// Notice sent the first time an encrypted event arrives in a room.
pub const ENCRYPTED_ROOM_NOTICE: &str =
    "Encrypted rooms are not supported. Liberado only reads unencrypted messages.";

#[cfg(test)]
mod tests {
    use super::{AcceptPolicy, Class, classify, in_user_namespace, parse_event};
    use serde_json::json;

    fn policy() -> AcceptPolicy {
        AcceptPolicy {
            owner: "@shiloh:example".into(),
            server_name: "example".into(),
            main_user: "@liberado:example".into(),
            sender_localpart: "liberado_bridge".into(),
        }
    }

    #[test]
    fn owner_messages_pass_and_other_people_do_not() {
        let policy = policy();
        assert_eq!(
            classify("@shiloh:example", "m.room.message", &policy),
            Class::Message
        );
        assert_eq!(
            classify("@Shiloh:example", "m.room.encrypted", &policy),
            Class::Encrypted
        );
        assert_eq!(
            classify("@other:example", "m.room.message", &policy),
            Class::Stranger
        );
        assert_eq!(
            classify("@shiloh:example", "m.room.member", &policy),
            Class::Other
        );
    }

    #[test]
    fn our_users_are_echoes() {
        let policy = policy();
        assert_eq!(
            classify("@liberado:example", "m.room.message", &policy),
            Class::Echo
        );
        assert_eq!(
            classify("@liberado_bridge:example", "m.room.message", &policy),
            Class::Echo
        );
        assert_eq!(
            classify("@liberado_coder:example", "m.room.message", &policy),
            Class::Echo
        );
        assert!(in_user_namespace("@Liberado:Example", "example"));
        assert!(!in_user_namespace("@liberado_:example", "example"));
        assert!(!in_user_namespace("@liberado_bad space:example", "example"));
    }

    #[test]
    fn the_namespace_match_is_the_whole_user_id() {
        assert!(in_user_namespace("@liberado:example", "example"));
        assert!(in_user_namespace("@liberado_coder:example", "example"));
        assert!(!in_user_namespace("prefix@liberado:example", "example"));
        assert!(!in_user_namespace("@liberado:example.suffix", "example"));
        assert!(!in_user_namespace(" @liberado:example", "example"));
        assert!(!in_user_namespace("@liberado:example ", "example"));
        assert!(!in_user_namespace("@liberado:example:extra", "example"));
    }

    #[test]
    fn parse_event_reads_the_message_body() {
        let event = parse_event(&json!({
            "type": "m.room.message",
            "sender": "@shiloh:example",
            "room_id": "!room:example",
            "content": { "msgtype": "m.text", "body": "status" }
        }))
        .expect("event");
        assert_eq!(event.body, "status");
        assert!(parse_event(&json!({"type": "m.room.message"})).is_none());
    }
}
