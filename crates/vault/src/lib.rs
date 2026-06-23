//! # liberado-vault
//!
//! Liberado's thin adapter over Turbovault. Two jobs, both load-bearing for background autonomy:
//!
//! 1. **Provenance-tagged writes** (Decision 5). Every agent write goes through
//!    `write_*_with_metadata`, attaching a [`WriteProvenance`] to the audit entry. This is the
//!    consumer side of the upstream `metadata` passthrough.
//! 2. **Consumer-side hash-join attribution** ([`attribution`]). Given an observed change, decide
//!    *react or suppress* by matching the file's content hash against the `after_hash` of a recent
//!    agent write (concurrency spec §6). This is what stops reactive ACPs from reacting to their
//!    own writes.
//!
//! It is also the single place the §8.1 upstream-dependency fallbacks are isolated (see
//! [`error`]). The daemon layers recency windows, correlation-set de-looping, and reaction-depth
//! limits on top of the primitives here.

mod attribution;
mod error;

pub use attribution::Attribution;
pub use error::{VaultError, VaultResult};

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::mpsc::UnboundedReceiver;
use turbovault_audit::{AuditLog, SnapshotStore};
use turbovault_core::prelude::{ServerConfig, VaultConfig};
use turbovault_vault::{VaultManager, VaultWatcher, WatcherConfig};

pub use liberado_common::WriteProvenance;
/// Re-exported so consumers (the daemon) can match on change events without depending on
/// Turbovault directly — this crate is the single boundary to Turbovault.
pub use turbovault_vault::VaultEvent;

/// A handle to a Turbovault-backed vault with the audit log enabled.
///
/// Cheap to clone (everything is `Arc`-shared), so the daemon, MCPs, and ACPs can hold their own
/// handles to the same vault.
#[derive(Clone)]
pub struct Vault {
    manager: Arc<VaultManager>,
    audit: Arc<AuditLog>,
    vault_root: PathBuf,
}

impl Vault {
    /// Open the vault at `path`, enabling the audit log + snapshot store (required for provenance
    /// and attribution). `name` is a label for the vault registration.
    pub async fn open(name: impl Into<String>, path: impl Into<PathBuf>) -> VaultResult<Self> {
        let vault_root = path.into();

        let mut config = ServerConfig::new();
        config
            .vaults
            .push(VaultConfig::builder(name, &vault_root).build()?);
        let mut manager = VaultManager::new(config)?;

        // Enable the audit trail. Provenance rides on audit entries, and the hash-join reads them
        // back — without this the loop-breaking machinery has nothing to join against.
        let audit = Arc::new(AuditLog::new(&vault_root).await?);
        let snapshots = Arc::new(SnapshotStore::new(audit.snapshot_dir().to_path_buf()));
        manager.set_audit_log(audit.clone(), snapshots.clone());

        Ok(Self {
            manager: Arc::new(manager),
            audit,
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
    /// fresh create / unconditional write). The provenance is attached to the audit entry so this
    /// write is attributable and loop-breakable.
    pub async fn write(
        &self,
        rel_path: impl AsRef<Path>,
        content: &str,
        expected_hash: Option<&str>,
        provenance: &WriteProvenance,
    ) -> VaultResult<()> {
        self.manager
            .write_file_with_metadata(
                rel_path.as_ref(),
                content,
                expected_hash,
                Some(provenance.to_audit_metadata()),
            )
            .await?;
        Ok(())
    }

    /// Delete a note with provenance.
    pub async fn delete(
        &self,
        rel_path: impl AsRef<Path>,
        expected_hash: Option<&str>,
        provenance: &WriteProvenance,
    ) -> VaultResult<()> {
        self.manager
            .delete_file_with_metadata(
                rel_path.as_ref(),
                expected_hash,
                Some(provenance.to_audit_metadata()),
            )
            .await?;
        Ok(())
    }

    /// Move/rename a note with provenance (wikilinks are updated by Turbovault).
    pub async fn move_note(
        &self,
        from: impl AsRef<Path>,
        to: impl AsRef<Path>,
        expected_hash: Option<&str>,
        provenance: &WriteProvenance,
    ) -> VaultResult<()> {
        self.manager
            .move_file_with_metadata(
                from.as_ref(),
                to.as_ref(),
                expected_hash,
                Some(provenance.to_audit_metadata()),
            )
            .await?;
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

    /// Access to the underlying audit log (used by [`attribution`]).
    pub(crate) fn audit(&self) -> &AuditLog {
        &self.audit
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
