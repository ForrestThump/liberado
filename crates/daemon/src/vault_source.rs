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
}

impl VaultEventSource {
    pub(crate) fn new(vault: Vault, debounce: Duration) -> Self {
        Self { vault, debounce }
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
                        match attribute_and_build_event(&self.vault, &rel).await {
                            Ok(Some(event)) => {
                                if tx.send(event).is_err() {
                                    return; // receiver gone
                                }
                            }
                            Ok(None) => {} // our own write or vanished path — suppressed
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
pub(crate) async fn attribute_and_build_event(
    vault: &Vault,
    rel_path: &Path,
) -> Result<Option<Event>, VaultError> {
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
