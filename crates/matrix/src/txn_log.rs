//! Transaction ids the homeserver has already delivered.
//!
//! The homeserver retries the same id. A repeat returns `{}` and does not run again.
//! The ids are stored under the data directory so a restart does not replay them.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const DEFAULT_CAP: usize = 256;

#[derive(Debug, Serialize, Deserialize)]
struct TxnFile {
    #[serde(default)]
    ids: Vec<String>,
}

/// Bounded set of processed transaction ids.
pub struct TxnLog {
    path: PathBuf,
    ids: std::sync::Mutex<VecDeque<String>>,
    cap: usize,
}

impl TxnLog {
    /// Load `path`. A missing or unreadable file starts empty.
    pub fn load(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        Self {
            ids: std::sync::Mutex::new(read_ids(&path)),
            path,
            cap: DEFAULT_CAP,
        }
    }

    /// Record `id` if it is new. `Ok(false)` means it was already recorded.
    /// A failed write removes the id again so the homeserver retry is not dropped.
    pub async fn record(&self, id: &str) -> Result<bool, String> {
        let (previous, snapshot) = {
            let mut ids = self.ids.lock().map_err(|_| "txn log lock".to_string())?;
            if ids.iter().any(|seen| seen == id) {
                return Ok(false);
            }
            let previous = ids.clone();
            ids.push_back(id.to_string());
            while ids.len() > self.cap {
                ids.pop_front();
            }
            let snapshot = ids.iter().cloned().collect::<Vec<_>>();
            (previous, snapshot)
        };
        if let Err(error) = write_ids(&self.path, &snapshot).await {
            restore_ids(self, previous);
            return Err(error);
        }
        Ok(true)
    }
}

fn restore_ids(log: &TxnLog, previous: VecDeque<String>) {
    if let Ok(mut ids) = log.ids.lock() {
        *ids = previous;
    }
}

fn read_ids(path: &Path) -> VecDeque<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return VecDeque::new();
    };
    match serde_json::from_str::<TxnFile>(&text) {
        Ok(file) => file.ids.into_iter().collect(),
        Err(_) => VecDeque::new(),
    }
}

async fn write_ids(path: &Path, ids: &[String]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| format!("matrix txn dir: {error}"))?;
    }
    let body = serde_json::to_string(&TxnFile { ids: ids.to_vec() })
        .map_err(|error| format!("matrix txn json: {error}"))?;
    tokio::fs::write(path, body)
        .await
        .map_err(|error| format!("matrix txn file: {error}"))
}

#[cfg(test)]
mod tests {
    use super::TxnLog;

    #[tokio::test]
    async fn a_repeat_is_not_new_and_a_restart_remembers_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("matrix-txns.json");
        let log = TxnLog::load(&path);
        assert!(log.record("txn-1").await.unwrap());
        assert!(!log.record("txn-1").await.unwrap());
        let reloaded = TxnLog::load(&path);
        assert!(!reloaded.record("txn-1").await.unwrap());
        assert!(reloaded.record("txn-2").await.unwrap());
    }

    #[tokio::test]
    async fn the_log_keeps_only_the_latest_ids() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("matrix-txns.json");
        let log = TxnLog {
            path: path.clone(),
            ids: std::sync::Mutex::new(std::collections::VecDeque::new()),
            cap: 2,
        };
        assert!(log.record("a").await.unwrap());
        assert!(log.record("b").await.unwrap());
        assert!(log.record("c").await.unwrap());
        let reloaded = TxnLog::load(&path);
        assert!(reloaded.record("a").await.unwrap());
        assert!(!reloaded.record("c").await.unwrap());
    }

    #[tokio::test]
    async fn a_failed_write_forgets_the_id_so_the_retry_is_not_a_repeat() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("not-a-file");
        std::fs::create_dir(&path).unwrap();
        let log = TxnLog::load(&path);
        assert!(log.record("txn-1").await.is_err());
        assert!(log.record("txn-1").await.is_err());
    }
}
