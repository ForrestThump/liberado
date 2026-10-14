//! Room creation body that keeps encryption off.
//!
//! `m.room.encryption` is irreversible. The human owner is power level 50. The encryption
//! event and the power-level event both require 100, so the owner cannot turn encryption on
//! or lower that requirement.
//!
//! Room version 12 gives the creator infinite power and rejects that user in `users`.
//! The create body asks for version 12 only when the homeserver lists it under
//! `m.room_versions.available`, and then omits the puppet. Older servers still list the
//! puppet at 100, because an override that drops `users` would leave the creator at
//! `users_default` (0).

use serde_json::{Value, json};

/// Whether `createRoom` lists the creating puppet in `power_levels.users`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CreatorPower {
    /// Room versions before 12. The puppet is power level 100.
    Listed,
    /// Room version 12. The creator is omitted and has unbounded power.
    Implicit,
}

/// Power-level content that locks `m.room.encryption` above the owner.
pub fn encryption_locked_power_levels(
    owner_mxid: &str,
    bot_mxid: &str,
    creator: CreatorPower,
) -> Value {
    let mut levels = json!({
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
            owner_mxid: 50
        }
    });
    if let CreatorPower::Listed = creator {
        levels["users"][bot_mxid] = json!(100);
    }
    levels
}

/// `createRoom` body for a later direct room. The main room already exists and is not created here.
pub fn direct_room_body(
    name: &str,
    invite_mxid: &str,
    owner_mxid: &str,
    bot_mxid: &str,
    creator: CreatorPower,
) -> Value {
    let mut body = json!({
        "preset": "private_chat",
        "is_direct": true,
        "name": name,
        "invite": [invite_mxid],
        "power_level_content_override": encryption_locked_power_levels(owner_mxid, bot_mxid, creator)
    });
    if let CreatorPower::Implicit = creator {
        body["room_version"] = json!("12");
    }
    body
}

/// `true` when `actor_mxid` cannot send `m.room.encryption`.
///
/// `creator_mxid` is the room creator on room version 12. That user is unbounded even
/// when `users` omits them, so the lock does not apply and the warning path stays quiet.
pub fn encryption_is_locked(
    power_levels: &Value,
    actor_mxid: &str,
    creator_mxid: Option<&str>,
) -> bool {
    if let Some(creator) = creator_mxid
        && creator == actor_mxid
    {
        return false;
    }
    event_level(power_levels, "m.room.encryption") > user_level(power_levels, actor_mxid)
}

/// `true` when `GET /capabilities` lists `version` in `m.room_versions.available`.
pub fn advertises_room_version(capabilities: &Value, version: &str) -> bool {
    let Some(caps) = capabilities.get("capabilities") else {
        return false;
    };
    let Some(versions) = caps.get("m.room_versions") else {
        return false;
    };
    let Some(available) = versions.get("available") else {
        return false;
    };
    available.get(version).is_some()
}

fn user_level(power_levels: &Value, user_id: &str) -> i64 {
    let Some(users) = power_levels.get("users") else {
        return 0;
    };
    let Some(level) = users.get(user_id) else {
        return 0;
    };
    level.as_i64().unwrap_or(0)
}

fn event_level(power_levels: &Value, event_type: &str) -> i64 {
    let Some(events) = power_levels.get("events") else {
        return 0;
    };
    let Some(level) = events.get(event_type) else {
        return 0;
    };
    level.as_i64().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        CreatorPower, advertises_room_version, direct_room_body, encryption_is_locked,
        encryption_locked_power_levels,
    };

    #[test]
    fn owner_cannot_enable_encryption() {
        let levels = encryption_locked_power_levels(
            "@shiloh:example",
            "@liberado:example",
            CreatorPower::Listed,
        );
        assert_eq!(levels["users"]["@shiloh:example"], 50);
        assert_eq!(levels["users"]["@liberado:example"], 100);
        assert_eq!(levels["events"]["m.room.encryption"], 100);
        assert_eq!(levels["events"]["m.room.power_levels"], 100);
        assert!(encryption_is_locked(&levels, "@shiloh:example", None));
        assert!(!encryption_is_locked(&levels, "@liberado:example", None));
        assert!(!encryption_is_locked(&json!({}), "@shiloh:example", None));
    }

    #[test]
    fn direct_room_body_uses_the_same_lock() {
        let body = direct_room_body(
            "Coder",
            "@shiloh:example",
            "@shiloh:example",
            "@bot:example",
            CreatorPower::Listed,
        );
        assert_eq!(body["preset"], "private_chat");
        assert_eq!(body["is_direct"], true);
        assert_eq!(body["invite"][0], "@shiloh:example");
        assert!(body.get("room_version").is_none());
        assert!(encryption_is_locked(
            &body["power_level_content_override"],
            "@shiloh:example",
            Some("@bot:example")
        ));
    }

    #[test]
    fn version_12_omits_the_creator_and_treats_that_user_as_unbounded() {
        let body = direct_room_body(
            "Coder",
            "@shiloh:example",
            "@shiloh:example",
            "@bot:example",
            CreatorPower::Implicit,
        );
        let levels = &body["power_level_content_override"];
        assert_eq!(body["room_version"], "12");
        assert!(levels["users"].get("@bot:example").is_none());
        assert_eq!(levels["users"]["@shiloh:example"], 50);
        assert!(!encryption_is_locked(
            levels,
            "@bot:example",
            Some("@bot:example")
        ));
        assert!(encryption_is_locked(
            levels,
            "@shiloh:example",
            Some("@bot:example")
        ));
        assert!(encryption_is_locked(
            levels,
            "@other:example",
            Some("@bot:example")
        ));
    }

    #[test]
    fn capabilities_advertise_room_version_12_only_when_it_is_listed() {
        let advertised = json!({
            "capabilities": {
                "m.room_versions": {
                    "default": "12",
                    "available": { "11": "stable", "12": "stable" }
                }
            }
        });
        let older = json!({
            "capabilities": {
                "m.room_versions": {
                    "default": "11",
                    "available": { "11": "stable" }
                }
            }
        });
        assert!(advertises_room_version(&advertised, "12"));
        assert!(!advertises_room_version(&older, "12"));
        assert!(!advertises_room_version(&json!({}), "12"));
    }
}
