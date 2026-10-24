//! Shared push hub. The daemon and the HTTP server hold the same `Arc`.

use super::send::{self, Delivery};
use super::store::{NewSubscription, Subscription, SubscriptionStore};
use super::vapid::{self, VapidMaterial};

/// Result of one fan-out. Counts only. No endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SendReport {
    pub sent: usize,
    pub failed: usize,
    pub removed: usize,
    pub skipped: usize,
    pub unconfigured: bool,
    pub missing: bool,
}

/// Web Push subscriptions and VAPID keys for this process.
pub struct PushHub {
    store: SubscriptionStore,
    keys: Option<VapidMaterial>,
}

impl PushHub {
    /// Load the file and the VAPID env vars. Missing or invalid keys stay inert.
    pub fn open(path: std::path::PathBuf) -> Self {
        let store = SubscriptionStore::open(path);
        match vapid::from_env() {
            Ok(keys) => Self {
                store,
                keys: Some(keys),
            },
            Err(err) => {
                tracing::info!("{err}");
                Self { store, keys: None }
            }
        }
    }

    /// No keys and no file. Quiet: this is the placeholder before the server wires the real hub.
    pub fn inert() -> Self {
        Self {
            store: SubscriptionStore::memory(),
            keys: None,
        }
    }

    /// A file with no keys. Subscribe still works. Sending does not.
    pub fn at_path(path: std::path::PathBuf) -> Self {
        Self {
            store: SubscriptionStore::open(path),
            keys: None,
        }
    }

    /// Keys from the caller. Tests use this so they do not read the process environment.
    pub fn with_keys(
        path: std::path::PathBuf,
        public_b64: &str,
        private_b64: &str,
        subject: &str,
    ) -> Result<Self, String> {
        let keys = vapid::from_parts(public_b64, private_b64, subject)?;
        Ok(Self {
            store: SubscriptionStore::open(path),
            keys: Some(keys),
        })
    }

    pub fn configured(&self) -> bool {
        self.keys.is_some()
    }

    pub fn vapid_public_key(&self) -> Option<String> {
        self.keys.as_ref().map(|keys| keys.public_b64().to_string())
    }

    pub fn upsert(&self, sub: NewSubscription) -> Result<(), String> {
        self.store.upsert(sub)
    }

    pub fn remove_endpoint(&self, endpoint: &str) -> Result<(), String> {
        self.store.remove_endpoint(endpoint)
    }

    pub fn subscription_count(&self) -> usize {
        self.store.count()
    }

    /// True when at least one subscription has not been sent `week_id`.
    pub fn needs_week(&self, week_id: &str) -> bool {
        self.store.needs_week(week_id)
    }

    pub async fn send_text(&self, endpoint: Option<&str>, title: &str, body: &str) -> SendReport {
        let Some(keys) = self.keys.as_ref() else {
            return report_unconfigured();
        };
        let rows = self.store.snapshot();
        let note = send::test_note(title, body);
        match targets(&rows, endpoint) {
            Target::Missing => report_missing(),
            Target::Rows(rows) => self.send_rows(keys, &rows, &note, None).await,
        }
    }

    pub async fn send_week(
        &self,
        week_id: &str,
        title: &str,
        body: &str,
        force: bool,
    ) -> SendReport {
        let Some(keys) = self.keys.as_ref() else {
            return report_unconfigured();
        };
        let rows = self.store.snapshot();
        let note = send::weekly(title, body, week_id);
        let mut report = SendReport {
            skipped: skipped_count(&rows, week_id, force),
            ..SendReport::default()
        };
        for sub in &rows {
            if due(sub, week_id, force) {
                self.one(&mut report, keys, sub, &note, Some(week_id)).await;
            }
        }
        report
    }

    async fn send_rows(
        &self,
        keys: &VapidMaterial,
        rows: &[Subscription],
        note: &send::Outgoing<'_>,
        week_id: Option<&str>,
    ) -> SendReport {
        let mut report = SendReport::default();
        for sub in rows {
            self.one(&mut report, keys, sub, note, week_id).await;
        }
        report
    }

    async fn one(
        &self,
        report: &mut SendReport,
        keys: &VapidMaterial,
        sub: &Subscription,
        note: &send::Outgoing<'_>,
        week_id: Option<&str>,
    ) {
        match send::deliver(keys, sub, note).await {
            Delivery::Ok => record_ok(report, &self.store, &sub.endpoint, week_id),
            Delivery::Gone => record_gone(report, &self.store, &sub.endpoint),
            Delivery::Failed => report.failed += 1,
        }
    }
}

fn report_unconfigured() -> SendReport {
    SendReport {
        unconfigured: true,
        ..SendReport::default()
    }
}

fn report_missing() -> SendReport {
    SendReport {
        missing: true,
        ..SendReport::default()
    }
}

enum Target {
    Missing,
    Rows(Vec<Subscription>),
}

fn targets(rows: &[Subscription], endpoint: Option<&str>) -> Target {
    match endpoint {
        None => Target::Rows(rows.to_vec()),
        Some(endpoint) => one_row(rows, endpoint),
    }
}

fn one_row(rows: &[Subscription], endpoint: &str) -> Target {
    for row in rows {
        if row.endpoint == endpoint {
            return Target::Rows(vec![row.clone()]);
        }
    }
    Target::Missing
}

fn skipped_count(rows: &[Subscription], week_id: &str, force: bool) -> usize {
    if force {
        return 0;
    }
    rows.iter().filter(|row| already_sent(row, week_id)).count()
}

fn due(sub: &Subscription, week_id: &str, force: bool) -> bool {
    force || !already_sent(sub, week_id)
}

fn already_sent(sub: &Subscription, week_id: &str) -> bool {
    sub.last_sent_week.as_deref() == Some(week_id)
}

fn record_ok(
    report: &mut SendReport,
    store: &SubscriptionStore,
    endpoint: &str,
    week_id: Option<&str>,
) {
    report.sent += 1;
    if let Some(week_id) = week_id {
        let _ = store.mark_sent(endpoint, week_id);
    }
}

fn record_gone(report: &mut SendReport, store: &SubscriptionStore, endpoint: &str) {
    report.removed += 1;
    let _ = store.remove_endpoint(endpoint);
}
