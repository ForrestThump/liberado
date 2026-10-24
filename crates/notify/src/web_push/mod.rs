//! Web Push (RFC 8291 + RFC 8292) for the weekly spending reminder.
//!
//! The private VAPID key comes from the environment. This module never logs it.
//! A midweek over-budget alert is a later change. This path does not call a model.

mod aesgcm;
mod b64;
mod hub;
mod send;
mod store;
mod vapid;

pub use hub::{PushHub, SendReport};
pub use store::NewSubscription;
pub use vapid::vapid_env_lines;

#[cfg(test)]
mod hub_tests {
    use super::*;
    use wiremock::matchers::{header, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const AS_PRIVATE: &str = "yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw";
    const AS_PUBLIC: &str =
        "BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8";
    const UA_PUBLIC: &str =
        "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";
    const AUTH: &str = "BTBZMqHH6r4Tts7J_aSIgg";

    fn sub(endpoint: &str) -> NewSubscription {
        NewSubscription {
            endpoint: endpoint.into(),
            p256dh: UA_PUBLIC.into(),
            auth: AUTH.into(),
            label: "  ".into(),
        }
    }

    #[tokio::test]
    async fn a_201_marks_the_week_and_a_410_removes_the_phone() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(header("content-encoding", "aes128gcm"))
            .and(header("content-type", "application/octet-stream"))
            .and(header("ttl", "86400"))
            .and(header("topic", "spend20261019"))
            .respond_with(ResponseTemplate::new(201))
            .expect(1)
            .mount(&server)
            .await;
        let path = std::env::temp_dir().join(format!("liberado-push-ok-{}", std::process::id()));
        let hub = PushHub::with_keys(
            path.clone(),
            AS_PUBLIC,
            AS_PRIVATE,
            "mailto:push@example.com",
        )
        .expect("keys");
        let endpoint = format!("{}/fake-subscription", server.uri());
        hub.upsert(sub(&endpoint)).expect("subscribe");
        assert_eq!(hub.subscription_count(), 1);
        let report = hub
            .send_week("2026-10-19", "Weekly spending", "Last week", false)
            .await;
        assert_eq!(report.sent, 1);
        assert!(!hub.needs_week("2026-10-19"));
        let again = hub
            .send_week("2026-10-19", "Weekly spending", "Last week", false)
            .await;
        assert_eq!(again.skipped, 1);
        assert_eq!(again.sent, 0);
        let _ = std::fs::remove_file(&path);

        let gone = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(410))
            .expect(1)
            .mount(&gone)
            .await;
        let gone_path =
            std::env::temp_dir().join(format!("liberado-push-gone-{}", std::process::id()));
        let hub = PushHub::with_keys(
            gone_path.clone(),
            AS_PUBLIC,
            AS_PRIVATE,
            "mailto:push@example.com",
        )
        .expect("keys");
        let endpoint = format!("{}/fake-subscription", gone.uri());
        hub.upsert(sub(&endpoint)).expect("subscribe");
        let report = hub
            .send_week("2026-10-19", "Weekly spending", "Last week", true)
            .await;
        assert_eq!(report.removed, 1);
        assert_eq!(hub.subscription_count(), 0);
        let _ = std::fs::remove_file(gone_path);
    }

    #[test]
    fn a_corrupt_file_starts_empty_and_a_bad_key_is_refused() {
        let path = std::env::temp_dir().join(format!("liberado-push-bad-{}", std::process::id()));
        std::fs::write(&path, b"not-json").expect("write");
        let hub = PushHub::at_path(path.clone());
        assert_eq!(hub.subscription_count(), 0);
        assert!(!hub.configured());
        let err = hub
            .upsert(NewSubscription {
                endpoint: "https://push.example.test/fake-subscription".into(),
                p256dh: "aaaa".into(),
                auth: AUTH.into(),
                label: "phone".into(),
            })
            .expect_err("key");
        assert!(err.contains("65-byte"), "{err}");
        let _ = std::fs::remove_file(path);
    }
}
