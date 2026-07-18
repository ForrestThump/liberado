//! # liberado-vault
//!
//! Liberado's thin adapter over Turbovault. Two jobs, both load-bearing for background autonomy:
//!
//! 1. **Provenance-tagged writes** (Decision 5). Every agent write goes through the plain Turbovault
//!    write API and is *also* recorded in a liberado-owned [`provenance_ledger`] as
//!    `(path, after_hash, provenance)`. The ledger — not Turbovault's audit metadata — is our
//!    provenance store: backend-agnostic and free of any Turbovault fork (see that module for why).
//! 2. **Consumer-side hash-join attribution** ([`attribution`]). Given an observed change, decide
//!    *react or suppress* by matching the file's content hash against the `after_hash` of a recent
//!    agent write in the ledger (concurrency spec §6). This is what stops reactive hooks from
//!    reacting to their own writes.
//!
//! It is also the single place the §8.1 upstream-dependency fallbacks are isolated (see
//! [`error`]). The daemon layers recency windows, correlation-set de-looping, and reaction-depth
//! limits on top of the primitives here.

mod attribution;
mod error;
mod provenance_ledger;
mod recording_runtime;

pub use attribution::Attribution;
pub use error::{VaultError, VaultResult};
pub use recording_runtime::{
    ProvenanceRecordingRuntime, RecordingRuntimeFactory, write_specs_from_descriptors,
};

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::mpsc::UnboundedReceiver;
use turbovault_audit::SnapshotStore;
use turbovault_core::prelude::{ServerConfig, VaultConfig};
use turbovault_vault::{VaultManager, VaultWatcher, WatcherConfig};

use crate::provenance_ledger::ProvenanceLedger;

pub use liberado_common::WriteProvenance;
/// Re-exported so consumers (the daemon) can match on change events without depending on
/// Turbovault directly — this crate is the single boundary to Turbovault.
pub use turbovault_vault::VaultEvent;

/// A handle to a Turbovault-backed vault plus liberado's provenance ledger.
///
/// Cheap to clone (everything is `Arc`-shared), so the daemon, MCPs, and hooks can hold their own
/// handles to the same vault *and the same ledger* — a write through one clone is attributable from
/// any other (e.g. the approval bot's write is seen by the daemon's watcher).
#[derive(Clone)]
pub struct Vault {
    manager: Arc<VaultManager>,
    ledger: Arc<ProvenanceLedger>,
    vault_root: PathBuf,
}

impl Vault {
    /// Open the vault at `path`, with the provenance ledger at `<data_dir>/provenance-ledger.jsonl`.
    /// The ledger path is intentionally **not** per-`name`: every Vault a daemon opens over its data
    /// dir (the daemon's own, the goal-session recorder, chat) must share one ledger so a write by
    /// any of them is visible to the daemon's attribution (see [`provenance_ledger`]).
    pub async fn open(name: impl Into<String>, path: impl Into<PathBuf>) -> VaultResult<Self> {
        let ledger_path = liberado_config::data_dir().join("provenance-ledger.jsonl");
        Self::open_with_ledger(name, path, ledger_path).await
    }

    /// Open with an explicit ledger path — used by tests for isolation (each test gets its own
    /// ledger in a temp dir instead of colliding on the shared data dir).
    pub async fn open_with_ledger(
        name: impl Into<String>,
        path: impl Into<PathBuf>,
        ledger_path: PathBuf,
    ) -> VaultResult<Self> {
        let vault_root = path.into();

        let mut config = ServerConfig::new();
        config
            .vaults
            .push(VaultConfig::builder(name, &vault_root).build()?);
        let manager = VaultManager::new(config)?;

        // Shared per ledger path (process-wide) so every Vault over the same ledger sees one tail.
        let ledger = ProvenanceLedger::open(ledger_path).await;

        Ok(Self {
            manager: Arc::new(manager),
            ledger,
            vault_root,
        })
    }

    /// The vault's root directory.
    pub fn root(&self) -> &Path {
        &self.vault_root
    }

    /// Read a note's raw content (including frontmatter).
    pub async fn read(&self, rel_path: impl AsRef<Path>) -> VaultResult<String> {
        Ok(self.manager.read_file(rel_path.as_ref()).await?)
    }

    /// The content hash Turbovault would record as `after_hash` for `content`. Exposed so callers
    /// can compute the `expected_hash` for an optimistic write without a re-read.
    pub fn content_hash(content: &str) -> String {
        SnapshotStore::compute_hash(content)
    }

    /// Write a note with provenance. `expected_hash` enables optimistic concurrency (`None` for a
    /// fresh create / unconditional write). The write goes through Turbovault's plain API and is
    /// recorded in the provenance ledger so it is attributable and loop-breakable.
    pub async fn write(
        &self,
        rel_path: impl AsRef<Path>,
        content: &str,
        expected_hash: Option<&str>,
        provenance: &WriteProvenance,
    ) -> VaultResult<()> {
        let rel_path = rel_path.as_ref();
        self.manager
            .write_file(rel_path, content, expected_hash)
            .await?;
        let hash = Self::content_hash(content);
        self.ledger.record(rel_path, Some(&hash), provenance).await;
        Ok(())
    }

    /// Delete a note with provenance. Recorded with no `after_hash` (no resulting content); the hash
    /// join skips it — a delete surfaces to attribution as `Missing`, which the caller resolves.
    pub async fn delete(
        &self,
        rel_path: impl AsRef<Path>,
        expected_hash: Option<&str>,
        provenance: &WriteProvenance,
    ) -> VaultResult<()> {
        let rel_path = rel_path.as_ref();
        self.manager.delete_file(rel_path, expected_hash).await?;
        self.ledger.record(rel_path, None, provenance).await;
        Ok(())
    }

    /// Move/rename a note with provenance (wikilinks are updated by Turbovault). The ledger records
    /// the *destination* path with the moved content's hash — the resulting bytes attribution will
    /// see there.
    pub async fn move_note(
        &self,
        from: impl AsRef<Path>,
        to: impl AsRef<Path>,
        expected_hash: Option<&str>,
        provenance: &WriteProvenance,
    ) -> VaultResult<()> {
        let to = to.as_ref();
        self.manager
            .move_file(from.as_ref(), to, expected_hash)
            .await?;
        // Record the destination's resulting content so a change event at `to` is attributed to us.
        if let Ok(content) = self.read(to).await {
            let hash = Self::content_hash(&content);
            self.ledger.record(to, Some(&hash), provenance).await;
        }
        Ok(())
    }

    /// Convert an absolute filesystem path (as delivered by the watcher) to a vault-relative
    /// path. Returns `None` if the path is outside the vault or cannot be resolved (e.g. it was
    /// deleted). Canonicalizes both sides so platform path quirks don't defeat the strip.
    pub fn to_relative(&self, absolute: &Path) -> Option<PathBuf> {
        let root = self.vault_root.canonicalize().ok()?;
        let abs = absolute.canonicalize().ok()?;
        abs.strip_prefix(&root).ok().map(Path::to_path_buf)
    }

    /// Start watching the vault filesystem for changes.
    ///
    /// This is the Decision 5 §8.1 *fallback* for Turbovault's not-yet-merged native subscription
    /// (PR #24): the daemon runs its own `notify` watcher and applies the same hash-join
    /// attribution. The returned [`VaultWatch`] must be kept alive for watching to continue. The
    /// watcher is markdown-only and ignores hidden files, so audit/snapshot writes under
    /// `.turbovault/` never surface as events.
    pub async fn watch(&self) -> VaultResult<VaultWatch> {
        let (mut watcher, events) =
            VaultWatcher::new(self.vault_root.clone(), WatcherConfig::default())?;
        watcher.start().await?;
        Ok(VaultWatch {
            _watcher: watcher,
            events,
        })
    }

    /// The provenance ledger backing [`attribution`].
    pub(crate) fn ledger(&self) -> &ProvenanceLedger {
        &self.ledger
    }

    /// Record a write performed *through an MCP* (not our in-process API) so it is attributable for
    /// loop-breaking. Reads the resulting bytes at `path` to compute the same `after_hash`
    /// attribution will later match; a path that no longer exists (a delete) is recorded with none.
    pub async fn record_external_write(
        &self,
        rel_path: impl AsRef<Path>,
        provenance: &WriteProvenance,
    ) {
        let rel_path = rel_path.as_ref();
        match self.read(rel_path).await {
            Ok(content) => {
                let hash = Self::content_hash(&content);
                self.ledger.record(rel_path, Some(&hash), provenance).await;
            }
            Err(_) => self.ledger.record(rel_path, None, provenance).await,
        }
    }
}

/// A live filesystem watch over the vault. Holds the watcher alive — drop it to stop watching.
pub struct VaultWatch {
    _watcher: VaultWatcher,
    events: UnboundedReceiver<VaultEvent>,
}

impl VaultWatch {
    /// Await the next vault change event, or `None` once the watcher has shut down.
    pub async fn next_event(&mut self) -> Option<VaultEvent> {
        self.events.recv().await
    }
}
