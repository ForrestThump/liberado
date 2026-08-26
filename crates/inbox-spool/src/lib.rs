//! The delegator's inbox (plan §9): an append-only spool of worker events, keyed for
//! exactly-once *handling* under at-least-once *delivery*.
//!
//! Physical shape: `<data>/delegate-inbox/items.jsonl` (every item ever, append-only)
//! plus `settled.log` (sequence numbers already drained). Dedupe is by the event's
//! correlation id, which the worker mints from a persisted monotonic counter — so a
//! replayed SSE stream or an adapter restart cannot double-enqueue a question.
//!
//! FIFO discipline: within one task, order is strict (the worker's sequence is
//! monotonic); across tasks, arrival order. `pending()` drains in sequence order and
//! skips settled items, so "drain the inbox" after downtime processes the backlog
//! oldest-first without re-reporting anything already handled.
//!
//! Nothing here knows about HTTP or the daemon: producers convert their own events,
//! consumers decide what draining means. This crate owns only the durable queue.

use std::collections::HashSet;
use std::io::Write as _;
use std::path::PathBuf;

/// What kind of attention an item asks for. Stable vocabulary: new kinds append.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    /// A parked run needs an answer.
    Question,
    /// A pull request is ready for review/merge.
    PrReady,
    /// The worker gave up on something and wants a human decision.
    Blocked,
    /// Milestone pings; off by default in adapters.
    Note,
}

/// One inbox entry. `seq` orders the spool; `correlation_id` dedupes it.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct InboxItem {
    pub seq: u64,
    pub kind: ItemKind,
    pub task_id: String,
    pub correlation_id: String,
    #[serde(default)]
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Appended {
    /// New item stored at this sequence number.
    Appended(u64),
    /// An item with this correlation id was already enqueued; nothing changed.
    Duplicate,
}

#[derive(Debug, thiserror::Error)]
pub enum SpoolError {
    #[error("spool io: {0}")]
    Io(#[from] std::io::Error),
    #[error("spool json for {context}: {source}")]
    Json {
        context: String,
        source: serde_json::Error,
    },
}

pub struct Spool {
    root: PathBuf,
    seen: HashSet<String>,
    settled: HashSet<u64>,
    next_seq: u64,
}

impl Spool {
    /// Open (creating if needed) the spool under `<data>/delegate-inbox`. Existing
    /// journals are loaded into memory; torn trailing lines — a crash mid-append —
    /// are skipped, matching the worker's journal semantics.
    pub fn open(data_dir: &std::path::Path) -> Result<Self, SpoolError> {
        let root = data_dir.join("delegate-inbox");
        std::fs::create_dir_all(&root)?;
        let mut spool = Self {
            root,
            seen: HashSet::new(),
            settled: HashSet::new(),
            next_seq: 1,
        };
        let items_path = spool.root.join("items.jsonl");
        if items_path.exists() {
            let raw = std::fs::read_to_string(&items_path)?;
            for line in raw.lines() {
                let Ok(item) = serde_json::from_str::<InboxItem>(line) else {
                    continue;
                };
                spool.seen.insert(item.correlation_id);
                spool.next_seq = spool.next_seq.max(item.seq + 1);
            }
        }
        let settled_path = spool.root.join("settled.log");
        if settled_path.exists() {
            let raw = std::fs::read_to_string(&settled_path)?;
            for line in raw.lines() {
                if let Ok(seq) = line.trim().parse::<u64>() {
                    spool.settled.insert(seq);
                }
            }
        }
        Ok(spool)
    }

    /// Enqueue one item unless its correlation id was already seen. Append wins over
    /// nothing: a duplicate returns [`Appended::Duplicate`] and writes no bytes.
    pub fn append(
        &mut self,
        kind: ItemKind,
        task_id: impl Into<String>,
        correlation_id: impl Into<String>,
        payload: serde_json::Value,
    ) -> Result<Appended, SpoolError> {
        let correlation_id = correlation_id.into();
        if self.seen.contains(&correlation_id) {
            return Ok(Appended::Duplicate);
        }
        let item = InboxItem {
            seq: self.next_seq,
            kind,
            task_id: task_id.into(),
            correlation_id: correlation_id.clone(),
            payload,
        };
        self.append_line("items.jsonl", &item)?;
        self.seen.insert(correlation_id);
        self.next_seq += 1;
        Ok(Appended::Appended(item.seq))
    }

    /// Unsettled items, oldest first. Order comes from the file itself, which is
    /// append order, which is sequence order.
    pub fn pending(&self) -> Result<Vec<InboxItem>, SpoolError> {
        Ok(self
            .read_items()?
            .into_iter()
            .filter(|item| !self.settled.contains(&item.seq))
            .collect())
    }

    pub fn pending_count(&self) -> Result<usize, SpoolError> {
        Ok(self.pending()?.len())
    }

    /// Mark one item handled. Settling is idempotent: a crash between drain and
    /// settle replays the item, and the consumer's own idempotency (keyed on
    /// correlation id) absorbs it — the same two-sided discipline everywhere else.
    pub fn settle(&mut self, seq: u64) -> Result<(), SpoolError> {
        if self.settled.contains(&seq) {
            return Ok(());
        }
        use std::fmt::Write as _;
        let mut line = String::new();
        let _ = writeln!(line, "{seq}");
        self.append_str("settled.log", &line)?;
        self.settled.insert(seq);
        Ok(())
    }

    fn read_items(&self) -> Result<Vec<InboxItem>, SpoolError> {
        let path = self.root.join("items.jsonl");
        if !path.exists() {
            return Ok(Vec::new());
        }
        let raw = std::fs::read_to_string(&path)?;
        Ok(raw
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect())
    }

    fn append_line<T: serde::Serialize>(&self, name: &str, value: &T) -> Result<(), SpoolError> {
        let line = serde_json::to_string(value).map_err(|source| SpoolError::Json {
            context: name.to_string(),
            source,
        })?;
        self.append_str(name, &format!("{line}\n"))
    }

    fn append_str(&self, name: &str, text: &str) -> Result<(), SpoolError> {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.root.join(name))?;
        file.write_all(text.as_bytes())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
