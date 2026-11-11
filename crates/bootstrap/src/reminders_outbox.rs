//! Reminder outbox wiring — kept out of `lib.rs` so cyclomatic complexity stays under its ratchet.

use std::sync::Arc;

use liberado_config::Config;
use liberado_cron::CronEventSource;
use liberado_daemon::Daemon;
use liberado_daemon::reminders::OutboxScheduleCursor;

pub fn attach_outbox(daemon: Daemon, config: &Config) -> Daemon {
    use liberado_daemon::reminders::OutboxDelivery;
    use liberado_outbox::Outbox;

    let path = liberado_config::data_dir().join(liberado_outbox::DB_FILE);
    let Ok(store) = Outbox::open(&path) else {
        tracing::error!(path = %path.display(), "reminder outbox failed to open");
        return daemon;
    };
    let outbox = Arc::new(store);
    let vapid = match liberado_notify::web_push::load_vapid() {
        Ok(keys) => keys,
        Err(err) => {
            tracing::warn!(error = %err, "VAPID keys could not be read");
            None
        }
    };
    if vapid.is_none() {
        tracing::info!(
            "Web Push off — set LIBERADO_VAPID_PUBLIC_KEY, LIBERADO_VAPID_PRIVATE_KEY, LIBERADO_VAPID_SUBJECT"
        );
    }
    let telegram = liberado_daemon::reminders::TelegramReminderSender::from_env();
    let delivery = Arc::new(OutboxDelivery::new(Arc::clone(&outbox), vapid, telegram));
    let reminders = config.tuning.reminders.clone();
    daemon.with_outbox(outbox, reminders, delivery)
}

pub fn cron_source_with_catch_up(
    cron_source: CronEventSource,
    daemon: &Daemon,
    config: &Config,
) -> CronEventSource {
    let Some(outbox) = daemon.outbox() else {
        return cron_source;
    };
    let secs = config
        .tuning
        .reminders
        .once_within_secs()
        .unwrap_or(6 * 3600);
    let window = chrono::Duration::seconds(i64::try_from(secs).unwrap_or(21_600));
    let store = Arc::new(OutboxScheduleCursor::new(outbox));
    let floor = Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
    cron_source.with_catch_up(liberado_cron::CatchUpSettings {
        window,
        store,
        floor,
    })
}
