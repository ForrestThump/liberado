//! [`VaultEventSource`] — the vault-watch [`EventSource`] implementation: the daemon's *first*
//! conformer to that trait (Decision 18/19), proving the seam against real, already-tested
//! behavior before cron becomes the second. This is exactly what `Daemon::run` did inline before
//! this module existed — raw filesystem watch, per-path debounce, hash-join attribution — moved,
//! not rewritten. [`crate::Daemon::process_change`] still delegates to
//! [`attribute_and_build_event`], so existing tests calling it directly are unaffected.

use std::path::Path;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use liberado_common::{Event, EventPayload, EventSource, event_source};
use liberado_vault::{Attribution, Vault, VaultError, VaultEvent};
use tokio::sync::mpsc::UnboundedSender;

use crate::VAULT_NOTE_CHANGED;
use crate::debounce::Debouncer;

/// Prefix of the correlation id minted for a vault-change reaction, and how many hex chars of the
/// content hash to append (enough to distinguish edits; the full id stays short for logs/journals).
const CORRELATION_PREFIX: &str = "vault-change";
const CORRELATION_HASH_LEN: usize = 12;

/// The vault-watch event source: raw filesystem watch → per-path debounce (coalescing a `notify`
/// burst into one settled change) → hash-join attribution (loop-breaking, Decision 5).
pub(crate) struct VaultEventSource {
    vault: Vault,
    debounce: Duration,
    ignore_globs: Vec<String>,
}

impl VaultEventSource {
    pub(crate) fn new(vault: Vault, debounce: Duration, ignore_globs: Vec<String>) -> Self {
        Self {
            vault,
            debounce,
            ignore_globs,
        }
    }
}

#[async_trait]
impl EventSource for VaultEventSource {
    fn name(&self) -> &str {
        "vault-watch"
    }

    async fn run(self: Box<Self>, tx: UnboundedSender<Event>) {
        let mut watch = match self.vault.watch().await {
            Ok(w) => w,
            Err(e) => {
                tracing::error!(error = %e, "vault-watch: failed to start watcher");
                return;
            }
        };
        let mut debouncer = Debouncer::new(self.debounce);
        let ignore_globs = self.ignore_globs;
        tracing::info!(
            vault = %self.vault.root().display(),
            debounce_ms = self.debounce.as_millis() as u64,
            "daemon watching vault"
        );

        loop {
            // Copy out the next deadline so the timer future borrows nothing from `debouncer`,
            // leaving the select arms free to mutate it.
            let next_deadline = debouncer.next_deadline();

            tokio::select! {
                maybe_event = watch.next_event() => {
                    let Some(event) = maybe_event else { break }; // watcher shut down
                    // Deletions carry no content to hash-join; reacting to them is a later iteration.
                    if let VaultEvent::FileDeleted(_) = event {
                        continue;
                    }
                    if let Some(rel) = self.vault.to_relative(event.path()) {
                        debouncer.observe(rel, Instant::now());
                    }
                }

                _ = sleep_until(next_deadline) => {
                    for rel in debouncer.drain_ready(Instant::now()) {
                        match attribute_and_build_event(&self.vault, &rel, &ignore_globs).await {
                            Ok(Some(event)) => {
                                if tx.send(event).is_err() {
                                    return; // receiver gone
                                }
                            }
                            Ok(None) => {} // our own write, vanished path, or ignored glob — suppressed
                            Err(e) => tracing::warn!(error = %e, ?rel, "attribution failed"),
                        }
                    }
                }
            }
        }
    }
}

/// The pure, deterministic attribution decision for a changed path: attribute, and build the
/// standardized [`Event`] for an external change. Shared between [`VaultEventSource::run`] and
/// [`crate::Daemon::process_change`] (kept as a thin public wrapper for direct testability).
///
/// `ignore_globs` are vault-relative glob patterns; a path matching any of them is dropped
/// without attribution — the same `Ok(None)` as an agent-authored or missing path.
pub(crate) async fn attribute_and_build_event(
    vault: &Vault,
    rel_path: &Path,
    ignore_globs: &[String],
) -> Result<Option<Event>, VaultError> {
    if matches_any_glob(rel_path, ignore_globs) {
        return Ok(None);
    }
    match vault.attribute(rel_path).await? {
        Attribution::External => match build_event(vault, rel_path).await {
            Ok(event) => Ok(Some(event)),
            Err(e) => {
                tracing::warn!(error = %e, ?rel_path, "vault read failed between attribution and event build — skipping change");
                Ok(None)
            }
        },
        Attribution::Agent(_) | Attribution::Missing => Ok(None),
    }
}

/// True when `rel_path` matches any pattern in `globs`. An empty list always returns `false`.
/// Patterns are matched against the vault-relative path string (with forward slashes).
fn matches_any_glob(rel_path: &Path, globs: &[String]) -> bool {
    if globs.is_empty() {
        return false;
    }
    let path_str = rel_path.to_string_lossy().replace('\\', "/");
    // Also check just the file name for patterns that are simple basename globs like `~*` or `*.tmp`.
    let file_name = rel_path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    globs.iter().any(|pattern| {
        // A trailing `/` means "match everything under this directory".
        let effective = if pattern.ends_with('/') {
            format!("{pattern}**")
        } else {
            pattern.clone()
        };
        glob::Pattern::new(&effective)
            .map(|p| p.matches(&path_str) || p.matches(file_name))
            .unwrap_or(false)
    })
}

/// Build the standardized event for an attributed-external change. The `correlation_id` keys
/// idempotency/loop-breaking downstream; it is derived from the path + a short content hash so
/// distinct edits are distinct events while a redelivery of the same state is not.
async fn build_event(vault: &Vault, rel_path: &Path) -> Result<Event, VaultError> {
    let content = vault.read(rel_path).await?;
    let hash = Vault::content_hash(&content);
    let rel = rel_path.to_string_lossy().replace('\\', "/");
    // `get(..N)` (not `[..N]`) is panic-safe regardless of the hash's byte boundaries.
    let short_hash = hash.get(..CORRELATION_HASH_LEN).unwrap_or(&hash);
    let correlation_id = format!("{CORRELATION_PREFIX}:{rel}:{short_hash}");
    Ok(Event::trigger(
        VAULT_NOTE_CHANGED,
        event_source::TURBOVAULT_SUBSCRIPTION,
        correlation_id,
        EventPayload {
            path: Some(rel),
            ..Default::default()
        },
    ))
}

/// Sleep until `deadline`, or forever when `None` (so the watch loop's select only wakes on
/// incoming events while nothing is pending).
async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => {
            tokio::time::sleep(deadline.saturating_duration_since(Instant::now())).await
        }
        None => std::future::pending::<()>().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a real on-disk vault in a temp dir, with one test file written directly
    /// (no audit entry, so `attribute()` returns `External`).
    async fn test_vault() -> (tempfile::TempDir, Vault, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let vault = Vault::open("test", dir.path()).await.expect("open vault");
        let note_rel = std::path::PathBuf::from("test-note.md");
        let note_abs = dir.path().join(&note_rel);
        std::fs::create_dir_all(note_abs.parent().unwrap()).ok();
        std::fs::write(&note_abs, "# hello").expect("write note");
        (dir, vault, note_rel)
    }

    #[tokio::test]
    async fn matching_glob_is_dropped() {
        let (_dir, vault, rel) = test_vault().await;
        let ignore_globs = vec!["*.md".to_string()];
        let result = attribute_and_build_event(&vault, &rel, &ignore_globs)
            .await
            .expect("should not error");
        assert!(result.is_none(), "path matching glob must be dropped");
    }

    #[tokio::test]
    async fn non_matching_glob_still_produces_event() {
        let (_dir, vault, rel) = test_vault().await;
        let ignore_globs = vec!["*.txt".to_string()];
        let result = attribute_and_build_event(&vault, &rel, &ignore_globs)
            .await
            .expect("should not error");
        assert!(
            result.is_some(),
            "non-matching path must still produce an event"
        );
    }

    #[tokio::test]
    async fn empty_glob_list_changes_nothing() {
        let (_dir, vault, rel) = test_vault().await;
        let result_no_globs = attribute_and_build_event(&vault, &rel, &[])
            .await
            .expect("should not error");
        let result_empty_vec = attribute_and_build_event(&vault, &rel, &Vec::new())
            .await
            .expect("should not error");
        assert!(result_no_globs.is_some(), "no globs → event");
        assert!(result_empty_vec.is_some(), "empty vec → event");
    }

    #[tokio::test]
    async fn agent_attributed_writes_are_still_dropped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let vault = Vault::open("test", dir.path()).await.expect("open vault");
        let rel = std::path::PathBuf::from("agent-note.md");
        let abs = dir.path().join(&rel);
        std::fs::create_dir_all(abs.parent().unwrap()).ok();
        // Write through the vault with Agent provenance so attribution sees it as Agent.
        let prov = liberado_common::WriteProvenance::agent("test-agent", "c1");
        vault
            .write(&rel, "# agent wrote this", None, &prov)
            .await
            .expect("vault write");
        // Even with empty ignore globs, agent-attributed writes must still be dropped.
        let result = attribute_and_build_event(&vault, &rel, &[])
            .await
            .expect("should not error");
        assert!(
            result.is_none(),
            "agent-attributed writes must be dropped regardless of globs"
        );
    }

    #[tokio::test]
    async fn glob_on_filename_matches() {
        let (dir, vault, _rel) = test_vault().await;
        // Create a file matching a basename glob (like `~*` for temp files).
        let rel = std::path::PathBuf::from("~tempfile.md");
        let abs = dir.path().join(&rel);
        std::fs::write(&abs, "# temp").expect("write temp");
        let ignore_globs = vec!["~*".to_string()];
        let result = attribute_and_build_event(&vault, &rel, &ignore_globs)
            .await
            .expect("should not error");
        assert!(
            result.is_none(),
            "basename glob `~*` must match ~tempfile.md"
        );
    }

    #[tokio::test]
    async fn glob_on_directory_matches() {
        let (dir, vault, _rel) = test_vault().await;
        let rel = std::path::PathBuf::from(".stversions/some-file.md");
        let abs = dir.path().join(&rel);
        std::fs::create_dir_all(abs.parent().unwrap()).ok();
        std::fs::write(&abs, "# versioned").expect("write versioned");
        let ignore_globs = vec![".stversions/".to_string()];
        let result = attribute_and_build_event(&vault, &rel, &ignore_globs)
            .await
            .expect("should not error");
        assert!(
            result.is_none(),
            "directory glob .stversions/ must match paths under .stversions/"
        );
    }
}
