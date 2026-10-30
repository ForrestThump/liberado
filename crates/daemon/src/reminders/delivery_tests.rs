//! Unit tests for outbox delivery readiness and Telegram env wiring.

use std::sync::Arc;

use liberado_outbox::{DeliverySink, Outbox};

use super::{OutboxDelivery, TelegramReminderSender};

const TEST_VAPID_PUBLIC: &str =
    "BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8";
const TEST_VAPID_PRIVATE: &str = "yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw";

fn open_outbox() -> Arc<Outbox> {
    let path = tempfile::NamedTempFile::new()
        .unwrap()
        .into_temp_path()
        .keep()
        .unwrap();
    Arc::new(Outbox::open(&path).unwrap())
}

struct EnvRestore {
    token: Option<std::ffi::OsString>,
    chat: Option<std::ffi::OsString>,
    vapid_public: Option<std::ffi::OsString>,
    vapid_private: Option<std::ffi::OsString>,
    vapid_subject: Option<std::ffi::OsString>,
}

impl EnvRestore {
    fn snapshot() -> Self {
        Self {
            token: std::env::var_os("LIBERADO_TELEGRAM_REMINDER_BOT_TOKEN"),
            chat: std::env::var_os("LIBERADO_TELEGRAM_REMINDER_CHAT_ID"),
            vapid_public: std::env::var_os("LIBERADO_VAPID_PUBLIC_KEY"),
            vapid_private: std::env::var_os("LIBERADO_VAPID_PRIVATE_KEY"),
            vapid_subject: std::env::var_os("LIBERADO_VAPID_SUBJECT"),
        }
    }

    fn restore(self) {
        // SAFETY(test): tests in this module run serially via the lock below.
        unsafe {
            restore_var("LIBERADO_TELEGRAM_REMINDER_BOT_TOKEN", self.token);
            restore_var("LIBERADO_TELEGRAM_REMINDER_CHAT_ID", self.chat);
            restore_var("LIBERADO_VAPID_PUBLIC_KEY", self.vapid_public);
            restore_var("LIBERADO_VAPID_PRIVATE_KEY", self.vapid_private);
            restore_var("LIBERADO_VAPID_SUBJECT", self.vapid_subject);
        }
    }
}

unsafe fn restore_var(key: &str, value: Option<std::ffi::OsString>) {
    unsafe {
        match value {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }
}

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn with_test_env<F: FnOnce()>(telegram: Option<(&str, &str)>, vapid: bool, f: F) {
    let _g = ENV_LOCK.lock().unwrap();
    let saved = EnvRestore::snapshot();
    // SAFETY(test): hold ENV_LOCK for the whole test closure.
    unsafe {
        std::env::remove_var("LIBERADO_TELEGRAM_REMINDER_BOT_TOKEN");
        std::env::remove_var("LIBERADO_TELEGRAM_REMINDER_CHAT_ID");
        std::env::remove_var("LIBERADO_VAPID_PUBLIC_KEY");
        std::env::remove_var("LIBERADO_VAPID_PRIVATE_KEY");
        std::env::remove_var("LIBERADO_VAPID_SUBJECT");
        if let Some((token, chat)) = telegram {
            std::env::set_var("LIBERADO_TELEGRAM_REMINDER_BOT_TOKEN", token);
            std::env::set_var("LIBERADO_TELEGRAM_REMINDER_CHAT_ID", chat);
        }
        if vapid {
            std::env::set_var("LIBERADO_VAPID_PUBLIC_KEY", TEST_VAPID_PUBLIC);
            std::env::set_var("LIBERADO_VAPID_PRIVATE_KEY", TEST_VAPID_PRIVATE);
            std::env::set_var("LIBERADO_VAPID_SUBJECT", "mailto:push-test@example.com");
        }
    }
    f();
    saved.restore();
}

#[test]
fn readiness_reports_webpush_gaps() {
    with_test_env(None, false, || {
        let delivery = OutboxDelivery::new(open_outbox(), None, None);
        let sink: &dyn DeliverySink = &delivery;
        assert!(sink.readiness("webpush", "sub:1").is_err());
    });
    with_test_env(None, true, || {
        let keys = liberado_notify::web_push::load_vapid()
            .expect("load")
            .expect("keys");
        let delivery = OutboxDelivery::new(open_outbox(), Some(keys), None);
        let sink: &dyn DeliverySink = &delivery;
        assert_eq!(
            sink.readiness("webpush", "").unwrap_err(),
            "no push subscriptions"
        );
        assert!(sink.readiness("webpush", "sub:9").is_ok());
    });
}

#[test]
fn readiness_covers_telegram_matrix_and_unknown() {
    with_test_env(Some(("test-bot-token", "999")), false, || {
        let telegram = TelegramReminderSender::from_env().expect("telegram");
        let delivery = OutboxDelivery::new(open_outbox(), None, Some(telegram));
        let sink: &dyn DeliverySink = &delivery;
        assert!(sink.readiness("telegram", "reminder").is_ok());
    });
    with_test_env(None, false, || {
        let delivery = OutboxDelivery::new(open_outbox(), None, None);
        let sink: &dyn DeliverySink = &delivery;
        assert_eq!(
            sink.readiness("telegram", "reminder").unwrap_err(),
            "reminder Telegram bot is not configured"
        );
        assert_eq!(
            sink.readiness("matrix", "!room:example").unwrap_err(),
            "matrix reminders are disabled"
        );
        assert!(sink.readiness("sms", "x").unwrap_err().contains("unknown"));
    });
}

#[test]
fn from_env_requires_both_non_empty_vars() {
    with_test_env(Some(("", "1")), false, || {
        assert!(TelegramReminderSender::from_env().is_none());
    });
    with_test_env(Some(("token", "  ")), false, || {
        assert!(TelegramReminderSender::from_env().is_none());
    });
    with_test_env(Some(("token", "42")), false, || {
        assert!(TelegramReminderSender::from_env().is_some());
    });
    with_test_env(None, false, || {
        assert!(TelegramReminderSender::from_env().is_none());
    });
}
