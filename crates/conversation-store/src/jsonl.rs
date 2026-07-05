//! The v1 [`ConversationStore`] implementation: one append-only JSONL file per conversation.
//!
//! On-disk layout is `<root>/<conversation_id>.jsonl`, where line 0 is the conversation header and
//! every subsequent line is one message node — each a single self-describing JSON [`Record`], so
//! the log stays greppable (`{"kind":"node","id":"01J...","message":{"role":"user",...}}`).
//!
//! The whole design hangs off one property: **ids are minted in append order by a single writer**,
//! so file order == id order with no on-disk index. Two locks make that hold under concurrency — a
//! global monotonic id generator, and a per-conversation lock held across the mint-and-write so a
//! given conversation's nodes can never interleave or race their ids. Reads are lock-free.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::error::{StoreError, StoreResult};
use crate::store::ConversationStore;
use crate::types::{ConversationHeader, MessageNode, NewConversation, NewNode};

/// One line of a conversation log. The `kind` tag is what lets a single file hold its header and
/// all its nodes while staying greppable and self-describing line by line.
///
/// Public (not `pub(crate)`) so a reader of the raw `.jsonl` files outside this crate —
/// `liberado-chat-search`'s directory scan is the motivating case — can deserialize a line into the
/// real, current shape instead of maintaining its own private mirror of this enum that a future new
/// variant here would silently fall out of sync with (`docs/roadmap/hygiene-audit-2026-07-05.md`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Record {
    Header(ConversationHeader),
    Node(MessageNode),
}

/// A daemonless conversation store backed by per-conversation JSONL files under `root`.
///
/// See the module docs for the layout and the single-writer guarantee. The on-disk root is the only
/// shared state, so multiple instances over the same root still see each other's durable writes —
/// only the in-process append locks are per-instance (wrap a single instance in an `Arc` to share
/// those locks across callers, which is what the concurrency guarantee assumes).
pub struct JsonlStore {
    root: PathBuf,
    /// The global, monotonic id source. Generating under one lock is what makes ids strictly
    /// increasing across the whole process, which the per-conversation append lock then turns into
    /// per-file sorted order.
    ids: Mutex<ulid::Generator>,
    /// Per-conversation append locks. Held across the entire mint-and-write so, for one
    /// conversation, mint-order == write-order (hence file order == id order). Different
    /// conversations take different locks, so they never contend.
    locks: Mutex<HashMap<Ulid, Arc<tokio::sync::Mutex<()>>>>,
}

impl JsonlStore {
    /// Open a store rooted at `root`, creating the directory if it does not yet exist. We create
    /// the root eagerly here so the first `create` does not have to special-case a missing dir.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        // Best-effort eager creation; a genuine failure resurfaces on the first write as an IO
        // error, so we do not panic here.
        let _ = std::fs::create_dir_all(&root);
        Self {
            root,
            ids: Mutex::new(ulid::Generator::new()),
            locks: Mutex::new(HashMap::new()),
        }
    }

    /// The path of a conversation's log file.
    fn path_for(&self, conversation: Ulid) -> PathBuf {
        self.root.join(format!("{conversation}.jsonl"))
    }

    /// Mint the next monotonic id. Monotonic generation is what gives the log its sorted-by-id
    /// property. [`Generator::generate`](ulid::Generator::generate) only errors on monotonic
    /// overflow — the same millisecond exhausting 80 bits of randomness — which is effectively
    /// unreachable; on that practically-never path we fall back to a fresh random [`Ulid`] rather
    /// than fail an append.
    fn mint_id(&self) -> Ulid {
        let mut generator = self.ids.lock().expect("id generator lock poisoned");
        generator.generate().unwrap_or_else(|_| Ulid::new())
    }

    /// Get (or insert) the append lock for a conversation. Cloned out so the caller can hold the
    /// inner `tokio::Mutex` across `.await` points without keeping the `locks` map locked.
    fn lock_for(&self, conversation: Ulid) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.locks.lock().expect("locks map poisoned");
        locks.entry(conversation).or_default().clone()
    }

    /// Read and parse a conversation log into (header, nodes), or `NotFound` if the file is absent.
    /// This is the lock-free read path shared by every query. A non-empty line that fails to parse
    /// is [`Corrupt`](StoreError::Corrupt), never silently dropped.
    async fn load(
        &self,
        conversation: Ulid,
    ) -> StoreResult<(ConversationHeader, Vec<MessageNode>)> {
        let path = self.path_for(conversation);
        let contents = match tokio::fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(StoreError::NotFound(format!("conversation {conversation}")));
            }
            Err(e) => return Err(e.into()),
        };

        let mut header: Option<ConversationHeader> = None;
        let mut nodes = Vec::new();
        for line in contents.split('\n') {
            if line.is_empty() {
                continue;
            }
            let record: Record = serde_json::from_str(line)?;
            match record {
                Record::Header(h) => {
                    if header.is_some() {
                        return Err(StoreError::Corrupt(format!(
                            "conversation {conversation} has more than one header record"
                        )));
                    }
                    nodes_must_be_empty(&nodes, conversation)?;
                    header = Some(h);
                }
                Record::Node(n) => nodes.push(n),
            }
        }

        match header {
            Some(h) => Ok((h, nodes)),
            None => Err(StoreError::Corrupt(format!(
                "conversation {conversation} log has no header record"
            ))),
        }
    }
}

/// Guard the "header is line 0" invariant: a node may not precede the header.
fn nodes_must_be_empty(nodes: &[MessageNode], conversation: Ulid) -> StoreResult<()> {
    if nodes.is_empty() {
        Ok(())
    } else {
        Err(StoreError::Corrupt(format!(
            "conversation {conversation} log has a node before its header"
        )))
    }
}

#[async_trait]
impl ConversationStore for JsonlStore {
    async fn create(&self, new: NewConversation) -> StoreResult<ConversationHeader> {
        let header = ConversationHeader {
            id: self.mint_id(),
            title: new.title,
            parent_conversation: new.parent_conversation,
            spawned_by: new.spawned_by,
            created_at: Utc::now(),
        };
        let path = self.path_for(header.id);
        let mut line = serde_json::to_string(&Record::Header(header.clone()))?;
        line.push('\n');
        // A freshly minted id cannot collide with an existing file, so a plain create/write is safe.
        tokio::fs::write(&path, line).await?;
        Ok(header)
    }

    async fn append(&self, conversation: Ulid, node: NewNode) -> StoreResult<MessageNode> {
        let lock = self.lock_for(conversation);
        let _guard = lock.lock().await;

        // Mint INSIDE the per-conversation lock: this is what guarantees mint-order == write-order
        // for this conversation, so the file stays sorted by id.
        let persisted = MessageNode {
            id: self.mint_id(),
            parent_id: node.parent_id,
            conversation_id: conversation,
            author: node.author,
            created_at: Utc::now(),
            message: node.message,
        };

        let mut line = serde_json::to_string(&Record::Node(persisted.clone()))?;
        line.push('\n');

        let path = self.path_for(conversation);
        let mut file = match tokio::fs::OpenOptions::new().append(true).open(&path).await {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(StoreError::NotFound(format!("conversation {conversation}")));
            }
            Err(e) => return Err(e.into()),
        };
        use tokio::io::AsyncWriteExt;
        file.write_all(line.as_bytes()).await?;
        Ok(persisted)
    }

    async fn leaf_path(
        &self,
        conversation: Ulid,
        leaf: Option<Ulid>,
    ) -> StoreResult<Vec<MessageNode>> {
        let (_header, nodes) = self.load(conversation).await?;
        let by_id: HashMap<Ulid, &MessageNode> = nodes.iter().map(|n| (n.id, n)).collect();

        // Pick the leaf to walk back from. An explicit id must exist; `None` means the conversation
        // current leaf — the greatest id, which is the last appended node.
        let leaf_node = match leaf {
            Some(id) => match by_id.get(&id) {
                Some(n) => *n,
                None => {
                    return Err(StoreError::NotFound(format!(
                        "node {id} in conversation {conversation}"
                    )));
                }
            },
            None => match nodes.iter().max_by_key(|n| n.id) {
                Some(n) => n,
                None => return Ok(Vec::new()),
            },
        };

        // Walk parent pointers root-ward, bounded by the node count so a malformed cycle cannot
        // loop forever — a parent is always an earlier (smaller) id, so a finite log has a finite
        // path.
        let mut path = Vec::new();
        let mut current = Some(leaf_node);
        while let Some(node) = current {
            path.push(node.clone());
            if path.len() > nodes.len() {
                return Err(StoreError::Corrupt(format!(
                    "conversation {conversation} has a parent cycle"
                )));
            }
            current = match node.parent_id {
                Some(pid) => Some(by_id.get(&pid).copied().ok_or_else(|| {
                    StoreError::Corrupt(format!(
                        "node {} in conversation {conversation} references missing parent {pid}",
                        node.id
                    ))
                })?),
                None => None,
            };
        }

        path.reverse();
        Ok(path)
    }

    async fn node(&self, conversation: Ulid, id: Ulid) -> StoreResult<Option<MessageNode>> {
        let (_header, nodes) = self.load(conversation).await?;
        Ok(nodes.into_iter().find(|n| n.id == id))
    }

    async fn children(&self, conversation: Ulid, id: Ulid) -> StoreResult<Vec<Ulid>> {
        let (_header, nodes) = self.load(conversation).await?;
        let mut children: Vec<Ulid> = nodes
            .iter()
            .filter(|n| n.parent_id == Some(id))
            .map(|n| n.id)
            .collect();
        children.sort();
        Ok(children)
    }

    async fn list(&self) -> StoreResult<Vec<ConversationHeader>> {
        let mut headers = Vec::new();
        let mut entries = match tokio::fs::read_dir(&self.root).await {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(headers),
            Err(e) => return Err(e.into()),
        };

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            // Only line 0 (the header) is needed here — read just that line instead of the whole
            // file, which can be sizeable once a conversation has accumulated many turns.
            let file = tokio::fs::File::open(&path).await?;
            let mut lines = tokio::io::AsyncBufReadExt::lines(tokio::io::BufReader::new(file));
            let Some(first) = lines.next_line().await? else {
                return Err(StoreError::Corrupt(format!(
                    "conversation log {} is empty",
                    path.display()
                )));
            };
            match serde_json::from_str::<Record>(&first)? {
                Record::Header(h) => headers.push(h),
                Record::Node(_) => {
                    return Err(StoreError::Corrupt(format!(
                        "conversation log {} does not start with a header",
                        path.display()
                    )));
                }
            }
        }

        // Newest first: ids are time-sortable, so a descending id sort is a descending recency sort.
        headers.sort_by(|a, b| b.id.cmp(&a.id));
        Ok(headers)
    }

    async fn set_title(&self, conversation: Ulid, title: String) -> StoreResult<()> {
        // Hold the same per-conversation lock `append` does: this rewrites the whole file from a
        // snapshot, so without the lock a concurrent append could land between the read and the
        // write and be silently dropped by the overwrite.
        let lock = self.lock_for(conversation);
        let _guard = lock.lock().await;

        let (existing, nodes) = self.load(conversation).await?;
        let updated = ConversationHeader {
            id: existing.id,
            title: Some(title),
            parent_conversation: existing.parent_conversation,
            spawned_by: existing.spawned_by,
            created_at: existing.created_at,
        };
        let mut contents = serde_json::to_string(&Record::Header(updated))?;
        contents.push('\n');
        for node in nodes {
            contents.push_str(&serde_json::to_string(&Record::Node(node))?);
            contents.push('\n');
        }
        // Write to a sibling temp file, then atomically rename over the real path — a crash between
        // the two leaves either the untouched original or the fully-written new content, never a
        // truncated file (a direct `tokio::fs::write` truncates-then-writes in place, so a crash
        // mid-write would lose the entire conversation, not just the title). `rename` replaces an
        // existing destination on both POSIX and Windows (`std::fs::rename` uses
        // `MOVEFILE_REPLACE_EXISTING` there), and staying in the same directory keeps it on one
        // filesystem, which is what makes the rename atomic.
        let path = self.path_for(conversation);
        let tmp_path = self.root.join(format!("{conversation}.jsonl.tmp"));
        tokio::fs::write(&tmp_path, contents).await?;
        tokio::fs::rename(&tmp_path, &path).await?;
        Ok(())
    }
}
