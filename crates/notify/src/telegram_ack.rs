//! Telegram `sendMessage` acceptance for the reminder outbox.
//!
//! HTTP 200 is not enough. The body has to say `"ok": true`. Approval cards keep
//! the older reader, which treats any HTTP success as acceptance.

/// True only when the status is 2xx and the JSON body has `"ok": true`.
pub fn telegram_send_accepted(status: u16, body: &str) -> bool {
    if !(200..300).contains(&status) {
        return false;
    }
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| value.get("ok").and_then(|flag| flag.as_bool()))
        == Some(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_200_with_ok_false_is_a_failure() {
        assert!(!telegram_send_accepted(
            200,
            r#"{"ok":false,"description":"no"}"#
        ));
        assert!(!telegram_send_accepted(200, "not-json"));
        assert!(!telegram_send_accepted(500, r#"{"ok":true}"#));
        assert!(telegram_send_accepted(
            200,
            r#"{"ok":true,"result":{"message_id":1}}"#
        ));
    }
}
