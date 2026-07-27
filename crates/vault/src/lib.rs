//! # liberado-vault
//!
//! Liberado's thin adapter over Turbovault. Two jobs, both load-bearing for background autonomy:
//!
//! 1. **Provenance-tagged writes** (Decision 5). Every agent write is issued as a `ChangePlan`
//!    carrying a [`WriteProvenance`] in its `metadata`, which the write substrate records on the
//!    resulting audit entry. This is the consumer side of the upstream `metadata` passthrough.
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
use turbovault_core::{Change, ChangePlan, Precondition};
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
    ///
    /// The **component filter is the defense**; the `starts_with` at the end is a belt-and-braces
    /// restatement, not load-bearing (a lexical `join` cannot escape once `..` is already gone).
    ///
    /// # Scope
    ///
    /// In scope: every path argument reaching this type's `read` / `write` / `delete` /
    /// `move_note`, which is the whole in-process surface — the daemon (proposal + reaper paths),
    /// `liberado-memory-store`, and `liberado-telegram-approvals` all go through here, and none
    /// construct absolute paths.
    ///
    /// Explicitly **not** in scope:
    ///
    /// - **Symlinks inside the vault that point outside it.** These checks are lexical, so a
    ///   symlink is followed by the OS after validation passes. The vault is an Obsidian
    ///   directory the operator owns; an attacker who can plant a symlink in it already has write
    ///   access to the source of truth. Closing this would need `canonicalize` on every call —
    ///   a syscall per operation, and still TOCTOU-racy.
    /// - **Agent-facing vault tools.** Those are MCP calls to the turbovault server, which does
    ///   its own path handling in its own process; they never pass through this wrapper. This
    ///   guard hardens *our* callers, and is not the sandbox for tool-driven writes.
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

    /// The path spelling a [`ChangePlan`] must use: Turbovault resolves and re-relativises a path
    /// before planning, and the plan's paths are what land in the audit entry — so attribution
    /// (which matches on that spelling) only works if we normalise identically.
    fn plan_path(&self, rel_path: &Path) -> VaultResult<String> {
        let resolved = self.manager.resolve_path(rel_path)?;
        Ok(self.manager.relative_path(&resolved))
    }

    /// Turn the adapter's `expected_hash` into the substrate's precondition. `Some` is optimistic
    /// concurrency (fail if the file changed under us); `None` keeps the documented "unconditional
    /// write" semantics — deliberately [`Precondition::Blind`] and not `ExpectAbsent`, which would
    /// turn every overwrite into an error.
    fn precondition(expected_hash: Option<&str>) -> Precondition {
        match expected_hash {
            Some(hash) => Precondition::ExpectBlob(hash.to_string()),
            None => Precondition::Blind,
        }
    }

    /// Write a note with provenance. `expected_hash` enables optimistic concurrency (`None` for a
    /// fresh create / unconditional write). The provenance rides on the plan's `metadata`, which
    /// the substrate records on the audit entry, so this write is attributable and loop-breakable.
    pub async fn write(
        &self,
        rel_path: impl AsRef<Path>,
        content: &str,
        expected_hash: Option<&str>,
        provenance: &WriteProvenance,
    ) -> VaultResult<()> {
        self.validate_rel_path(rel_path.as_ref())?;
        let path = self.plan_path(rel_path.as_ref())?;
        let plan = ChangePlan::new(format!("write {path}"))
            .upsert(path.clone(), content.as_bytes())
            .with_precondition(path, Self::precondition(expected_hash))
            .with_metadata(provenance.to_audit_metadata());
        self.manager.apply_changes(&plan).await?;
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
        let path = self.plan_path(rel_path.as_ref())?;
        let plan = ChangePlan::new(format!("delete {path}"))
            .remove(path.clone())
            .with_precondition(path, Self::precondition(expected_hash))
            .with_metadata(provenance.to_audit_metadata());
        self.manager.apply_changes(&plan).await?;
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
        let from_path = self.plan_path(from.as_ref())?;
        let to_path = self.plan_path(to.as_ref())?;
        let plan = ChangePlan::new(format!("move {from_path} to {to_path}"))
            .with_change(Change::Rename {
                from: from_path.clone(),
                to: to_path.clone(),
            })
            .with_precondition(from_path, Self::precondition(expected_hash))
            // Never clobber the destination — matches `ChangePlan::rename`'s own semantics.
            .with_precondition(to_path, Precondition::ExpectAbsent)
            .with_metadata(provenance.to_audit_metadata());
        self.manager.apply_changes(&plan).await?;
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
    use tempfile::TempDir;
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

    #[tokio::test]
    async fn validate_rel_path_rejects_traversal_out_of_the_vault() {
        let dir = TempDir::new().unwrap();
        let vault = Vault::open("test", dir.path()).await.unwrap();

        for escape in [
            "../etc/passwd",
            "notes/../../../etc/passwd",
            "notes/sub/../../etc/passwd",
            "notes/../..",
            "..",
        ] {
            assert!(
                matches!(
                    vault.validate_rel_path(Path::new(escape)),
                    Err(VaultError::PathTraversal(_))
                ),
                "must reject traversal: {escape}"
            );
        }
    }

    #[tokio::test]
    async fn validate_rel_path_allows_normal_relative_paths() {
        let dir = TempDir::new().unwrap();
        let vault = Vault::open("test", dir.path()).await.unwrap();

        for ok in [
            "notes/hello.md",
            "proposals/abc-123.md",
            "hello.md",
            "sub/dir/file.md",
            "./notes/hello.md", // a CurDir component is not an escape
        ] {
            assert!(
                vault.validate_rel_path(Path::new(ok)).is_ok(),
                "must allow ordinary relative path: {ok}"
            );
        }
    }

    #[tokio::test]
    async fn validate_rel_path_rejects_rooted_and_absolute_paths() {
        let dir = TempDir::new().unwrap();
        let vault = Vault::open("test", dir.path()).await.unwrap();

        // Unix-absolute; on Windows this is a RootDir component rather than `is_absolute`, and
        // both arms reject — which is the point of matching on `Component`, not string prefixes.
        assert!(matches!(
            vault.validate_rel_path(Path::new("/etc/passwd")),
            Err(VaultError::PathTraversal(_))
        ));
        #[cfg(windows)]
        {
            // A Windows Prefix component (drive or UNC) must be rejected too.
            assert!(matches!(
                vault.validate_rel_path(Path::new(r"C:\Windows\System32\config\SAM")),
                Err(VaultError::PathTraversal(_))
            ));
            assert!(matches!(
                vault.validate_rel_path(Path::new(r"\\server\share\file.md")),
                Err(VaultError::PathTraversal(_))
            ));
        }
    }
}
