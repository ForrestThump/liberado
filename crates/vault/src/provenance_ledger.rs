//! A liberado-owned provenance ledger — the backing store for loop-break attribution.
//!
//! Historically provenance rode on Turbovault's audit-log entries via a custom `write_*_with_metadata`
//! passthrough. That coupled us to Turbovault's write internals (a fork carry) and to one write
//! backend. This ledger replaces it: `crates/vault` records every write **it** performs
//! `(resulting_path, after_hash, provenance)` into a small append-only log under the liberado data
//! dir, and [`attribution`](crate::attribution) hash-joins against *this* — not the audit log.
//!
//! Why this is more broadly compatible:
//! - **Backend-agnostic** — independent of whether Turbovault writes via its legacy path or a future
//!   git substrate; the ledger only sees liberado's own writes.
//! - **No Turbovault fork** — nothing to re-apply on an upstream rebase.
//! - **Human edits still resolve correctly** — an Obsidian edit never passes through `crates/vault`,
//!   so it is absent from the ledger → no hash match → attributed `External` → react (correct).
//!
//! The ledger keeps a bounded tail (recent writes are all attribution needs — it runs right after a
//! change) in memory, and mirrors it to disk so it survives a restart. On open it loads and compacts
//! the file to that tail, so it never grows without bound.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};

use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

use crate::WriteProvenance;

/// How many recent write records to retain. Attribution runs immediately after a change, so the
/// explaining write is always among the most recent; 512 is generous headroom over a burst.
const CAP: usize = 512;

/// Process-wide registry of open ledgers keyed by file path. Every `Vault` opened over the same
/// ledger path (the daemon's own vault, the goal-session orchestrators' recorder, the chat surface —
/// all in one process) shares **one** ledger instance, so a write recorded by any of them is visible
/// to the daemon's attribution in memory (not just after a file reload). Different paths — including
/// the per-test temp ledgers — get independent instances, preserving isolation.
static LEDGERS: OnceLock<StdMutex<HashMap<PathBuf, Arc<ProvenanceLedger>>>> = OnceLock::new();

fn registry() -> &'static StdMutex<HashMap<PathBuf, Arc<ProvenanceLedger>>> {
    LEDGERS.get_or_init(|| StdMutex::new(HashMap::new()))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LedgerRecord {
    /// Resulting vault-relative path, `/`-normalized (a move records its destination).
    path: String,
    /// The content hash Turbovault records as `after_hash` — i.e. `Vault::content_hash` of the bytes
    /// this write produced. `None` for a delete (no resulting content to match).
    after_hash: Option<String>,
    provenance: WriteProvenance,
    /// Unix seconds; advisory (ordering is by ledger position, newest last).
    ts: u64,
}

/// A bounded, disk-backed record of the writes `crates/vault` has performed, queried by attribution.
pub struct ProvenanceLedger {
    file: PathBuf,
    recent: Mutex<VecDeque<LedgerRecord>>,
}

impl ProvenanceLedger {
    /// Open (or reuse) the shared ledger for `file`. The first open for a path loads it from disk;
    /// subsequent opens of the same path return the same instance (see [`registry`]) so all writers
    /// and the daemon's attribution share one in-memory tail.
    pub async fn open(file: PathBuf) -> Arc<Self> {
        if let Some(existing) = registry().lock().unwrap().get(&file).cloned() {
            return existing;
        }
        // Load from disk outside the registry lock (async), then insert — or, if another task raced
        // us to it, discard ours and take theirs so there is exactly one instance per path.
        let loaded = Arc::new(Self::load(file.clone()).await);
        registry()
            .lock()
            .unwrap()
            .entry(file)
            .or_insert(loaded)
            .clone()
    }

    /// Load a ledger from disk, compacting its retained tail. A missing file starts empty. Parse
    /// errors on individual lines are skipped (a truncated line from a crash is ignored) rather than
    /// failing the daemon boot. Not registry-shared — [`open`](Self::open) is the shared entry point.
    async fn load(file: PathBuf) -> Self {
        let mut recent = VecDeque::new();
        if let Ok(contents) = tokio::fs::read_to_string(&file).await {
            for line in contents.lines().filter(|l| !l.trim().is_empty()) {
                if let Ok(rec) = serde_json::from_str::<LedgerRecord>(line) {
                    recent.push_back(rec);
                    if recent.len() > CAP {
                        recent.pop_front();
                    }
                }
            }
        }
        // Compact the on-disk file down to the retained tail so it can't grow unbounded across runs.
        if let Some(parent) = file.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        let mut body: String = recent
            .iter()
            .filter_map(|r| serde_json::to_string(r).ok())
            .collect::<Vec<_>>()
            .join("\n");
        if !body.is_empty() {
            body.push('\n');
        }
        let _ = tokio::fs::write(&file, body).await;

        Self {
            file,
            recent: Mutex::new(recent),
        }
    }

    /// Record a write. `after_hash` is `Some` for content-producing writes (write/move) and `None`
    /// for a delete. Appends to disk (best-effort) and to the in-memory tail.
    pub async fn record(&self, path: &Path, after_hash: Option<&str>, provenance: &WriteProvenance) {
        let rec = LedgerRecord {
            path: normalize(&path.to_string_lossy()),
            after_hash: after_hash.map(|h| h.to_string()),
            provenance: provenance.clone(),
            ts: now_secs(),
        };

        if let Ok(line) = serde_json::to_string(&rec) {
            if let Ok(mut f) = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.file)
                .await
            {
                let _ = f.write_all(line.as_bytes()).await;
                let _ = f.write_all(b"\n").await;
            }
        }

        let mut recent = self.recent.lock().await;
        recent.push_back(rec);
        while recent.len() > CAP {
            recent.pop_front();
        }
    }

    /// The provenance of the most recent write whose resulting content matches `(path, hash)` — i.e.
    /// the write that explains the current bytes on disk — or `None` if no ledgered write produced
    /// them (an external/human edit).
    pub async fn attribute(&self, path: &str, hash: &str) -> Option<WriteProvenance> {
        let target = normalize(path);
        let recent = self.recent.lock().await;
        recent
            .iter()
            .rev()
            .find(|r| r.path == target && r.after_hash.as_deref() == Some(hash))
            .map(|r| r.provenance.clone())
    }
}

/// Normalize path separators so attribution is cross-platform (callers/events may use `/`).
fn normalize(path: &str) -> String {
    path.replace('\\', "/")
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn ledger_path(dir: &TempDir) -> PathBuf {
        dir.path().join("provenance-ledger.jsonl")
    }

    #[tokio::test]
    async fn records_and_attributes_an_agent_write() {
        let dir = TempDir::new().unwrap();
        let ledger = ProvenanceLedger::open(ledger_path(&dir)).await;
        let prov = WriteProvenance::agent("daemon", "c1");
        ledger
            .record(Path::new("notes/a.md"), Some("hash1"), &prov)
            .await;

        // A `/`-spelled query matches an OS-separator record and returns the provenance.
        let got = ledger.attribute("notes/a.md", "hash1").await;
        assert_eq!(got, Some(prov));
        // A different hash (content changed since) does not match.
        assert_eq!(ledger.attribute("notes/a.md", "hash2").await, None);
        // A different path does not match.
        assert_eq!(ledger.attribute("notes/b.md", "hash1").await, None);
    }

    #[tokio::test]
    async fn survives_reopen_from_disk() {
        let dir = TempDir::new().unwrap();
        let path = ledger_path(&dir);
        {
            // `load` (not the registry-cached `open`) so this genuinely exercises disk persistence.
            let ledger = ProvenanceLedger::load(path.clone()).await;
            ledger
                .record(Path::new("n.md"), Some("h"), &WriteProvenance::agent("a", "c"))
                .await;
        }
        // A fresh load reads the persisted record back from disk.
        let reloaded = ProvenanceLedger::load(path).await;
        assert!(reloaded.attribute("n.md", "h").await.is_some());
    }

    #[tokio::test]
    async fn latest_write_wins_for_the_same_path() {
        let dir = TempDir::new().unwrap();
        let ledger = ProvenanceLedger::open(ledger_path(&dir)).await;
        ledger
            .record(Path::new("n.md"), Some("old"), &WriteProvenance::agent("a", "c"))
            .await;
        ledger
            .record(Path::new("n.md"), Some("new"), &WriteProvenance::human())
            .await;
        // Only the record whose hash matches the current bytes explains them.
        assert!(ledger.attribute("n.md", "new").await.unwrap().is_human());
        assert!(!ledger.attribute("n.md", "old").await.unwrap().is_human());
    }
}
