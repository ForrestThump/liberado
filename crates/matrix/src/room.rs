//! Room creation body that keeps encryption off.
//!
//! `m.room.encryption` is irreversible. The human owner is power level 50. The encryption
//! event and the power-level event both require 100, so the owner cannot turn encryption on
//! or lower that requirement. The bot user is 100 and does not send `m.room.encryption`.

use serde_json::{Value, json};

/// Power-level content that locks `m.room.encryption` above the owner.
pub fn encryption_locked_power_levels(owner_mxid: &str, bot_mxid: &str) -> Value {
    json!({
        "ban": 50,
        "kick": 50,
        "invite": 50,
        "redact": 50,
        "events_default": 0,
        "state_default": 50,
        "users_default": 0,
        "events": {
            "m.room.encryption": 100,
            "m.room.power_levels": 100
        },
        "users": {
            owner_mxid: 50,
            bot_mxid: 100
        }
    })
}

/// `createRoom` body for a later direct room. The main room already exists and is not created here.
pub fn direct_room_body(name: &str, invite_mxid: &str, owner_mxid: &str, bot_mxid: &str) -> Value {
    json!({
        "preset": "private_chat",
        "is_direct": true,
        "name": name,
        "invite": [invite_mxid],
        "power_level_content_override": encryption_locked_power_levels(owner_mxid, bot_mxid)
    })
}

/// `true` when the encryption event requires a level above the owner's level.
pub fn encryption_is_locked(power_levels: &Value, owner_mxid: &str) -> bool {
    let owner = power_levels
        .get("users")
        .and_then(|users| users.get(owner_mxid))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let required = power_levels
        .get("events")
        .and_then(|events| events.get("m.room.encryption"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    required > owner
}

#[cfg(test)]
mod tests {
    use super::{direct_room_body, encryption_is_locked, encryption_locked_power_levels};

    #[test]
    fn owner_cannot_enable_encryption() {
        let levels = encryption_locked_power_levels("@shiloh:example", "@liberado:example");
        assert_eq!(levels["users"]["@shiloh:example"], 50);
        assert_eq!(levels["users"]["@liberado:example"], 100);
        assert_eq!(levels["events"]["m.room.encryption"], 100);
        assert_eq!(levels["events"]["m.room.power_levels"], 100);
        assert!(encryption_is_locked(&levels, "@shiloh:example"));
        assert!(!encryption_is_locked(&levels, "@liberado:example"));
    }

    #[test]
    fn direct_room_body_uses_the_same_lock() {
        let body = direct_room_body(
            "Coder",
            "@shiloh:example",
            "@shiloh:example",
            "@bot:example",
        );
        assert_eq!(body["preset"], "private_chat");
        assert_eq!(body["is_direct"], true);
        assert_eq!(body["invite"][0], "@shiloh:example");
        assert!(encryption_is_locked(
            &body["power_level_content_override"],
            "@shiloh:example"
        ));
    }
}
