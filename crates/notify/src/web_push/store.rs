//! Subscription file. The endpoint path is a capability. Logs name the host only.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use super::b64;

const MAX_SUBS: usize = 32;
const MAX_ENDPOINT: usize = 2000;
const MAX_LABEL: usize = 80;

#[derive(Debug, Clone)]
pub struct NewSubscription {
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
    pub label: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Subscription {
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
    pub label: String,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_sent_week: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct FileBody {
    subscriptions: Vec<Subscription>,
}

pub(crate) struct SubscriptionStore {
    path: PathBuf,
    inner: Mutex<Vec<Subscription>>,
}

impl SubscriptionStore {
    pub(crate) fn open(path: PathBuf) -> Self {
        let inner = load(&path);
        Self {
            path,
            inner: Mutex::new(inner),
        }
    }

    pub(crate) fn memory() -> Self {
        Self {
            path: PathBuf::new(),
            inner: Mutex::new(Vec::new()),
        }
    }

    pub(crate) fn upsert(&self, incoming: NewSubscription) -> Result<(), String> {
        let prepared = prepare(incoming)?;
        let mut rows = lock(&self.inner);
        upsert_row(&mut rows, prepared)?;
        save(&self.path, &rows)
    }

    pub(crate) fn remove_endpoint(&self, endpoint: &str) -> Result<(), String> {
        let mut rows = lock(&self.inner);
        rows.retain(|row| row.endpoint != endpoint);
        save(&self.path, &rows)
    }

    pub(crate) fn count(&self) -> usize {
        lock(&self.inner).len()
    }

    pub(crate) fn needs_week(&self, week_id: &str) -> bool {
        lock(&self.inner)
            .iter()
            .any(|row| row.last_sent_week.as_deref() != Some(week_id))
    }

    pub(crate) fn snapshot(&self) -> Vec<Subscription> {
        lock(&self.inner).clone()
    }

    pub(crate) fn mark_sent(&self, endpoint: &str, week_id: &str) -> Result<(), String> {
        let mut rows = lock(&self.inner);
        mark_row(&mut rows, endpoint, week_id);
        save(&self.path, &rows)
    }
}

fn lock(inner: &Mutex<Vec<Subscription>>) -> std::sync::MutexGuard<'_, Vec<Subscription>> {
    inner.lock().unwrap_or_else(|poison| poison.into_inner())
}

fn upsert_row(rows: &mut Vec<Subscription>, prepared: Subscription) -> Result<(), String> {
    if let Some(existing) = rows
        .iter_mut()
        .find(|row| row.endpoint == prepared.endpoint)
    {
        existing.p256dh = prepared.p256dh;
        existing.auth = prepared.auth;
        existing.label = prepared.label;
        return Ok(());
    }
    if rows.len() >= MAX_SUBS {
        return Err("too many web push subscriptions".into());
    }
    rows.push(prepared);
    Ok(())
}

fn mark_row(rows: &mut [Subscription], endpoint: &str, week_id: &str) {
    for row in rows {
        if row.endpoint == endpoint {
            row.last_sent_week = Some(week_id.to_string());
        }
    }
}

fn prepare(incoming: NewSubscription) -> Result<Subscription, String> {
    let endpoint = require_endpoint(&incoming.endpoint)?;
    require_key(&incoming.p256dh)?;
    require_auth(&incoming.auth)?;
    Ok(Subscription {
        endpoint,
        p256dh: incoming.p256dh.trim().to_string(),
        auth: incoming.auth.trim().to_string(),
        label: clean_label(&incoming.label),
        created_at: chrono::Utc::now().to_rfc3339(),
        last_sent_week: None,
    })
}

fn require_endpoint(endpoint: &str) -> Result<String, String> {
    let endpoint = endpoint.trim();
    if endpoint.len() > MAX_ENDPOINT || endpoint.chars().any(char::is_whitespace) {
        return Err("endpoint is not a usable http(s) URL".into());
    }
    if endpoint.starts_with("https://") || endpoint.starts_with("http://") {
        return Ok(endpoint.to_string());
    }
    Err("endpoint must be an http(s) URL".into())
}

fn require_key(p256dh: &str) -> Result<(), String> {
    let bytes = b64::decode(p256dh).map_err(|_| key_error())?;
    if bytes.len() == 65 && bytes.first() == Some(&0x04) {
        return Ok(());
    }
    Err(key_error())
}

fn key_error() -> String {
    "subscription key is not a 65-byte P-256 key".into()
}

fn require_auth(auth: &str) -> Result<(), String> {
    let bytes = b64::decode(auth).map_err(|_| auth_error())?;
    if bytes.len() >= 16 {
        return Ok(());
    }
    Err(auth_error())
}

fn auth_error() -> String {
    "subscription auth secret is too short".into()
}

fn clean_label(label: &str) -> String {
    let trimmed = label.trim();
    if trimmed.is_empty() {
        return "phone".to_string();
    }
    trimmed.chars().take(MAX_LABEL).collect()
}

fn load(path: &Path) -> Vec<Subscription> {
    match fs::read(path) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(err) => {
            tracing::warn!(%err, "web push subscription file is unreadable; starting empty");
            Vec::new()
        }
        Ok(bytes) => parse_or_empty(&bytes),
    }
}

fn parse_or_empty(bytes: &[u8]) -> Vec<Subscription> {
    match serde_json::from_slice::<FileBody>(bytes) {
        Ok(body) => body.subscriptions,
        Err(_) => {
            tracing::warn!("web push subscription file is corrupt; starting empty");
            Vec::new()
        }
    }
}

fn save(path: &Path, rows: &[Subscription]) -> Result<(), String> {
    let body = serde_json::to_vec_pretty(&FileBody {
        subscriptions: rows.to_vec(),
    })
    .map_err(|_| "could not encode subscriptions".to_string())?;
    let tmp = temp_path(path);
    fs::write(&tmp, body).map_err(|_| "could not write subscriptions".to_string())?;
    replace_file(&tmp, path).map_err(|_| "could not replace subscriptions".to_string())
}

fn temp_path(path: &Path) -> PathBuf {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    PathBuf::from(tmp)
}

fn replace_file(tmp: &Path, dest: &Path) -> std::io::Result<()> {
    if fs::rename(tmp, dest).is_ok() {
        return Ok(());
    }
    let _ = fs::remove_file(dest);
    fs::rename(tmp, dest)
}

pub(crate) fn host_of(endpoint: &str) -> String {
    match origin(endpoint) {
        Ok(value) => value.split("://").nth(1).unwrap_or("unknown").to_string(),
        Err(_) => "unknown".to_string(),
    }
}

pub(crate) fn origin(endpoint: &str) -> Result<String, String> {
    let (scheme, rest) = scheme_and_rest(endpoint)?;
    let host = rest.split('/').next().unwrap_or("");
    if host.is_empty() {
        return Err("endpoint has no host".into());
    }
    Ok(format!("{scheme}://{host}"))
}

fn scheme_and_rest(endpoint: &str) -> Result<(&str, &str), String> {
    if let Some(rest) = endpoint.strip_prefix("https://") {
        return Ok(("https", rest));
    }
    match endpoint.strip_prefix("http://") {
        Some(rest) => Ok(("http", rest)),
        None => Err("endpoint must be an http(s) URL".into()),
    }
}
