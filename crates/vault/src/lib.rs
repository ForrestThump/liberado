//! # liberado-vault
//!
//! Liberado's thin adapter over Turbovault. Two jobs, both load-bearing for background autonomy:
//!
//! 1. **Provenance-tagged writes** (Decision 5). Every agent write goes through
//!    `write_*_with_metadata`, attaching a [`WriteProvenance`] to the audit entry. This is the
//!    consumer side of the upstream `metadata` passthrough.
//! 2. **Consumer-side hash-join attribution** ([`attribution`]). Given an observed change, decide
//!    *react or suppress* by matching the file's content hash against the `after_hash` of a recent
//!    agent write (concurrency spec §6). This is what stops reactive hooks from reacting to their
//!    own writes.
//!
//! It is also the single place the §8.1 upstream-dependency fallbacks are isolated (see
//! [`error`]). The daemon layers recency windows, correlation-set de-looping, and reaction-depth
//! limits on top of the primitives here.

mod attribution;
mod error;

pub use attribution::Attribution;
pub use error::{VaultError, VaultResult};

use std::path::{Component, Path, PathBuf};
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
/// Cheap to clone (everything is `Arc`-shared), so the daemon, MCPs, and hooks can hold their own
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

    /// Reject relative paths that attempt to escape the vault root — `..` components, absolute
    /// paths, and Windows path prefixes. Cross-platform: uses `std::path::Component` which
    /// correctly identifies `ParentDir` and `Prefix` on both Windows and Linux.
    fn validate_rel_path(&self, rel_path: &Path) -> VaultResult<()> {
        if rel_path.is_absolute() {
            return Err(VaultError::PathTraversal(format!(
                "absolute path rejected: {}",
                rel_path.display()
            )));
        }
        for component in rel_path.components() {
            match component {
                Component::ParentDir => {
                    return Err(VaultError::PathTraversal(format!(
                        "path traversal '..' rejected in: {}",
                        rel_path.display()
                    )));
                }
                Component::Prefix(_) => {
                    return Err(VaultError::PathTraversal(format!(
                        "Windows path prefix rejected in: {}",
                        rel_path.display()
                    )));
                }
                Component::RootDir => {
                    return Err(VaultError::PathTraversal(format!(
                        "rooted path rejected: {}",
                        rel_path.display()
                    )));
                }
                Component::Normal(_) | Component::CurDir => {}
            }
        }
        let resolved = self.vault_root.join(rel_path);
        if !resolved.starts_with(&self.vault_root) {
            return Err(VaultError::PathTraversal(format!(
                "resolved path outside vault: {}",
                rel_path.display()
            )));
        }
        Ok(())
    }

    /// Read a note's raw content (including frontmatter).
    pub async fn read(&self, rel_path: impl AsRef<Path>) -> VaultResult<String> {
        self.validate_rel_path(rel_path.as_ref())?;
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
        self.validate_rel_path(rel_path.as_ref())?;
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
        self.validate_rel_path(rel_path.as_ref())?;
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
        self.validate_rel_path(from.as_ref())?;
        self.validate_rel_path(to.as_ref())?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    #[cfg(test)]
    impl VaultWatch {
        fn from_receiver(events: UnboundedReceiver<VaultEvent>) -> Self {
            let (fake_watcher, _rx) =
                VaultWatcher::new(std::path::PathBuf::new(), WatcherConfig::default())
                    .expect("fake watcher creation");
            Self {
                _watcher: fake_watcher,
                events,
            }
        }
    }

    #[tokio::test]
    async fn next_event_yields_sent_event() {
        let (tx, rx) = mpsc::unbounded_channel();
        let event = VaultEvent::FileModified(std::path::PathBuf::from("test.md"));
        tx.send(event.clone()).unwrap();
        let mut watch = VaultWatch::from_receiver(rx);
        assert_eq!(watch.next_event().await, Some(event));
    }

    #[tokio::test]
    async fn next_event_returns_none_after_sender_drops() {
        let (tx, rx) = mpsc::unbounded_channel::<VaultEvent>();
        drop(tx);
        let mut watch = VaultWatch::from_receiver(rx);
        assert_eq!(watch.next_event().await, None);
    }

    #[test]
    fn validate_rel_path_rejects_absolute_paths() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let dir = std::env::temp_dir().join("validate-test-abs");
        let vault = rt.block_on(Vault::open("test", &dir)).unwrap();
        let _clean = Cleanup(Some(dir));

        assert!(matches!(
            vault.validate_rel_path(Path::new("../etc/passwd")),
            Err(VaultError::PathTraversal(_))
        ));
        assert!(matches!(
            vault.validate_rel_path(Path::new("notes/../../../etc/passwd")),
            Err(VaultError::PathTraversal(_))
        ));
        assert!(matches!(
            vault.validate_rel_path(Path::new("notes/sub/../../etc/passwd")),
            Err(VaultError::PathTraversal(_))
        ));
        assert!(matches!(
            vault.validate_rel_path(Path::new("notes/../..")),
            Err(VaultError::PathTraversal(_))
        ));
        assert!(matches!(
            vault.validate_rel_path(Path::new("..")),
            Err(VaultError::PathTraversal(_))
        ));
    }

    #[test]
    fn validate_rel_path_allows_normal_relative_paths() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let dir = std::env::temp_dir().join("validate-test-ok");
        let vault = rt.block_on(Vault::open("test", &dir)).unwrap();
        let _clean = Cleanup(Some(dir));

        assert!(vault.validate_rel_path(Path::new("notes/hello.md")).is_ok());
        assert!(
            vault
                .validate_rel_path(Path::new("proposals/abc-123.md"))
                .is_ok()
        );
        assert!(vault.validate_rel_path(Path::new("hello.md")).is_ok());
        assert!(
            vault
                .validate_rel_path(Path::new("sub/dir/file.md"))
                .is_ok()
        );
    }

    #[test]
    fn validate_rel_path_rejects_absolute_paths_linux() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let dir = std::env::temp_dir().join("validate-test-abs2");
        let vault = rt.block_on(Vault::open("test", &dir)).unwrap();
        let _clean = Cleanup(Some(dir));

        assert!(matches!(
            vault.validate_rel_path(Path::new("/etc/passwd")),
            Err(VaultError::PathTraversal(_))
        ));
    }

    struct Cleanup(Option<PathBuf>);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            if let Some(dir) = self.0.take() {
                let _ = std::fs::remove_dir_all(&dir);
            }
        }
    }
}
