//! [`VaultEventSource`] — the vault-watch [`EventSource`] implementation: the daemon's *first*
//! conformer to that trait (Decision 18/19), proving the seam against real, already-tested
//! behavior before cron becomes the second. This is exactly what `Daemon::run` did inline before
//! this module existed — raw filesystem watch, per-path debounce, hash-join attribution — moved,
//! not rewritten. [`crate::Daemon::process_change`] still delegates to
//! [`attribute_and_build_event`], so existing tests calling it directly are unaffected.
//!
//! F12 (positive scope): the watcher only emits an event for notes under a configured capture
//! path, notes that contain the ready flag (`#now` / `#ready-now`), or notes under `proposals/`
//! (the approval pipeline). `#hold-off` parks a note for both the watcher and the schedule.
//! Unflagged notes outside capture paths belong to the schedule and are dropped here.

use std::path::Path;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use liberado_common::{Event, EventPayload, EventSource, PROPOSALS_DIR, event_source};
use liberado_vault::{Attribution, Vault, VaultError, VaultEvent};
use tokio::sync::mpsc::UnboundedSender;

use crate::VAULT_NOTE_CHANGED;
use crate::debounce::Debouncer;
use crate::types::PROPOSALS_ARCHIVE_DIR;

/// Prefix of the correlation id minted for a vault-change reaction, and how many hex chars of the
/// content hash to append (enough to distinguish edits; the full id stays short for logs/journals).
const CORRELATION_PREFIX: &str = "vault-change";
const CORRELATION_HASH_LEN: usize = 12;

/// Positive scope for the vault watcher (F12).
///
/// `capture_paths` is a whitelist: folder prefixes (`inbox/`) and/or globs (`*.md`,
/// `Inbox/Capture.md`). The ready flag anywhere in the vault promotes a note; the hold flag
/// parks it. Active `proposals/` notes are always watcher-owned.
#[derive(Clone)]
pub(crate) struct CaptureScope {
    pub(crate) capture_paths: Vec<String>,
    pub(crate) ready_flag: String,
    pub(crate) hold_flag: String,
}

impl CaptureScope {
    pub(crate) fn new(
        capture_paths: impl IntoIterator<Item = impl Into<String>>,
        ready_flag: impl Into<String>,
        hold_flag: impl Into<String>,
    ) -> Self {
        Self {
            capture_paths: capture_paths.into_iter().map(Into::into).collect(),
            ready_flag: ready_flag.into(),
            hold_flag: hold_flag.into(),
        }
    }

    /// Production default: the spec's inbox folder plus the shipped flag names.
    pub(crate) fn production_default() -> Self {
        Self::new(["inbox/"], "#ready-now", "#hold-off")
    }
}

/// The vault-watch event source: raw filesystem watch → per-path debounce (coalescing a `notify`
/// burst into one settled change) → hash-join attribution (loop-breaking, Decision 5) →
/// positive-scope filter (F12: only capture paths, `#now`, and proposals produce events).
pub(crate) struct VaultEventSource {
    vault: Vault,
    debounce: Duration,
    ignore_globs: Vec<String>,
    scope: CaptureScope,
}

impl VaultEventSource {
    pub(crate) fn new(
        vault: Vault,
        debounce: Duration,
        ignore_globs: Vec<String>,
        scope: CaptureScope,
    ) -> Self {
        Self {
            vault,
            debounce,
            ignore_globs,
            scope,
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
        let scope = self.scope;
        tracing::info!(
            vault = %self.vault.root().display(),
            debounce_ms = self.debounce.as_millis() as u64,
            capture_paths = ?scope.capture_paths,
            "daemon watching vault (positive scope active)"
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
                        match attribute_and_build_event(&self.vault, &rel, &ignore_globs, &scope).await {
                            Ok(Some(event)) => {
                                if tx.send(event).is_err() {
                                    return; // receiver gone
                                }
                            }
                            Ok(None) => {} // our own write, vanished path, ignored glob, or out of scope
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
///
/// `scope` is the watcher's positive scope (F12, inbox-spec.md §14.2).
pub(crate) async fn attribute_and_build_event(
    vault: &Vault,
    rel_path: &Path,
    ignore_globs: &[String],
    scope: &CaptureScope,
) -> Result<Option<Event>, VaultError> {
    if matches_any_glob(rel_path, ignore_globs) {
        return Ok(None);
    }
    match vault.attribute(rel_path).await? {
        Attribution::External => {
            let content = match vault.read(rel_path).await {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(error = %e, ?rel_path, "vault read failed between attribution and event build — skipping change");
                    return Ok(None);
                }
            };
            if !is_in_watcher_scope(rel_path, &content, scope) {
                return Ok(None);
            }
            Ok(Some(build_event(rel_path, &content)))
        }
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

/// True when the note is the watcher's to own (F12 positive scope).
///
/// - An active `proposals/` note is always watcher-owned: `react()` special-cases it into the
///   proposal approval pipeline before any dispatch. The archive subtree is excluded — archived
///   notes are terminal and must never re-enter the pipeline.
/// - `#hold-off` (hold flag) parks a note for the watcher *and* the schedule — it always wins,
///   even inside a capture path or alongside `#now`.
/// - A note matching any configured capture path is watcher-owned with no flag required.
/// - `#now` (ready flag) anywhere in the vault promotes the note to watcher ownership.
/// - Anything else belongs to the schedule, not the watcher.
fn is_in_watcher_scope(rel_path: &Path, content: &str, scope: &CaptureScope) -> bool {
    // Active proposals are watcher-owned regardless of flags or location — the human's edit is
    // the approval authorization. Mirror react()'s own exclusion shape (path normalized to
    // forward slashes; skip the exact dir; skip the archive subtree).
    let rel = rel_path.to_string_lossy().replace('\\', "/");
    if rel.starts_with(PROPOSALS_DIR)
        && rel != PROPOSALS_DIR
        && !rel.starts_with(PROPOSALS_ARCHIVE_DIR)
    {
        return true;
    }
    if !scope.hold_flag.is_empty() && content.contains(scope.hold_flag.as_str()) {
        return false;
    }
    if scope
        .capture_paths
        .iter()
        .any(|pattern| matches_capture_entry(rel_path, pattern))
    {
        return true;
    }
    if !scope.ready_flag.is_empty() && content.contains(scope.ready_flag.as_str()) {
        return true;
    }
    false
}

/// True when `rel_path` matches one capture-path entry. Glob metacharacters (`*`, `?`, `[`)
/// use the same matcher as ignore globs; everything else is a folder/file prefix so `inbox/`
/// never matches `inbox2/`.
fn matches_capture_entry(rel_path: &Path, pattern: &str) -> bool {
    if pattern.contains('*') || pattern.contains('?') || pattern.contains('[') {
        matches_any_glob(rel_path, &[pattern.to_string()])
    } else {
        is_under_capture_path(rel_path, pattern)
    }
}

/// True when `rel_path` sits under the configured capture folder (e.g. `inbox/`). Component-wise
/// prefix match on forward-slash-normalized paths, so `inbox2/` never matches `inbox/`.
fn is_under_capture_path(rel_path: &Path, capture_path: &str) -> bool {
    let path_str = rel_path.to_string_lossy().replace('\\', "/");
    let capture = capture_path.trim_end_matches('/');
    if capture.is_empty() {
        return false;
    }
    path_str == capture || path_str.starts_with(&format!("{capture}/"))
}

/// Build the standardized event for an attributed-external change. The `correlation_id` keys
/// idempotency/loop-breaking downstream; it is derived from the path + a short content hash so
/// distinct edits are distinct events while a redelivery of the same state is not.
fn build_event(rel_path: &Path, content: &str) -> Event {
    let hash = Vault::content_hash(content);
    let rel = rel_path.to_string_lossy().replace('\\', "/");
    // `get(..N)` (not `[..N]`) is panic-safe regardless of the hash's byte boundaries.
    let short_hash = hash.get(..CORRELATION_HASH_LEN).unwrap_or(&hash);
    let correlation_id = format!("{CORRELATION_PREFIX}:{rel}:{short_hash}");
    Event::trigger(
        VAULT_NOTE_CHANGED,
        event_source::TURBOVAULT_SUBSCRIPTION,
        correlation_id,
        EventPayload {
            path: Some(rel),
            ..Default::default()
        },
    )
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
#[path = "vault_source_tests.rs"]
mod vault_source_tests;
