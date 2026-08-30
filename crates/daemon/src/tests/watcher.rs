//! Tests for vault watcher lifecycle, health, burst coalescing, and suppression.

use super::super::*;
use super::test_fixtures::*;
use liberado_common::{Event, EventPayload, WriteProvenance, event_source};
use std::path::Path;
use std::time::Duration;
use tokio::sync::mpsc::unbounded_channel;

/// `watcher_health()` starts false and is only true once `run()` has actually spawned the watch.
///
/// `GET /api/status` used to answer this with the literal `true`, so every dashboard asserted a
/// live capture pipeline whether or not one was running — which reads as "the pipeline broke" to
/// anyone debugging, rather than "it was never started". The flag is only worth anything if it can
/// be false, so that is what this pins.
#[tokio::test]
async fn watcher_health_is_false_before_run_spawns_the_watch() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::open("test", dir.path()).await.unwrap();
    let health = daemon.watcher_health();
    assert!(
        !health.load(std::sync::atomic::Ordering::Relaxed),
        "a daemon that has not run yet is not watching anything"
    );
    // The handle is shared, not a snapshot — the surface holds this across `run()` taking `self`.
    assert!(std::sync::Arc::ptr_eq(&health, &daemon.watcher_health()));
}

#[tokio::test]
async fn external_change_produces_reaction() {
    let (daemon, dir) = temp_daemon().await;
    // A human writes a note directly into the capture path (not through the adapter) —
    // no matching audit entry.
    std::fs::create_dir_all(dir.path().join("inbox")).unwrap();
    std::fs::write(dir.path().join("inbox/note.md"), "a human wrote this").unwrap();

    let event = daemon
        .process_change(Path::new("inbox/note.md"))
        .await
        .unwrap()
        .expect("external change should produce a reaction");
    assert_eq!(event.event_type, VAULT_NOTE_CHANGED);
    assert_eq!(event.source, event_source::TURBOVAULT_SUBSCRIPTION);
    assert_eq!(event.payload.path.as_deref(), Some("inbox/note.md"));
    assert!(event.is_reactable());
}

#[tokio::test]
async fn our_own_write_is_suppressed() {
    let (daemon, _dir) = temp_daemon().await;
    let prov = WriteProvenance::agent("tasks-mcp", "c1");
    daemon
        .vault()
        .write("tasks/today.md", "- [ ] x", None, &prov)
        .await
        .unwrap();

    assert!(
        daemon
            .process_change(Path::new("tasks/today.md"))
            .await
            .unwrap()
            .is_none(),
        "agent write must not trigger a reaction"
    );
}

#[tokio::test]
async fn missing_path_is_suppressed() {
    let (daemon, _dir) = temp_daemon().await;
    assert!(
        daemon
            .process_change(Path::new("nope.md"))
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn watcher_coalesces_burst_into_single_reaction() {
    let (daemon, dir) = temp_daemon().await;
    let daemon = daemon.with_debounce(Duration::from_millis(80));
    let vault_dir = dir.path().to_path_buf();
    let (tx, mut rx) = unbounded_channel();
    let handle = tokio::spawn(daemon.run(tx));

    // Give the watcher a moment to establish before writing into the capture path.
    tokio::time::sleep(Duration::from_millis(300)).await;
    std::fs::create_dir_all(vault_dir.join("inbox")).unwrap();
    std::fs::write(
        vault_dir.join("inbox/captured.md"),
        "dropped in from Obsidian",
    )
    .unwrap();

    // Exactly one reaction, despite notify firing Create + Modify + ... for one write.
    let reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for reaction")
        .expect("reaction channel closed");
    assert_eq!(
        reaction.event.payload.path.as_deref(),
        Some("inbox/captured.md")
    );
    assert!(
        matches!(reaction.outcome, ReactionOutcome::Observed),
        "watch-only: no dispatcher attached"
    );

    // No duplicate arrives within a generous margin past the debounce window.
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(
        rx.try_recv().is_err(),
        "the notify burst should have coalesced into a single reaction"
    );

    handle.abort();
}

#[tokio::test]
async fn event_sender_lets_an_external_producer_inject_an_event() {
    // The seam `liberado-server`'s webhook handler uses: grab a sender before `run()` moves
    // `self`, then push an `Event` in from completely outside any `EventSource` — no cron, no
    // vault change, just a direct injection — and it must still flow through `react()` exactly
    // like any other source.
    let (daemon, _dir) = temp_daemon().await;
    let sender = daemon.event_sender();

    let (tx, mut rx) = unbounded_channel();
    let handle = tokio::spawn(daemon.run(tx));

    tokio::time::sleep(Duration::from_millis(200)).await;
    sender
        .send(Event::trigger(
            "WebhookFired",
            "webhook:test-hook",
            "webhook:test-hook:1",
            EventPayload {
                summary: Some("an externally-injected goal".into()),
                ..Default::default()
            },
        ))
        .unwrap();

    let reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for the injected event's reaction")
        .expect("reaction channel closed");

    assert_eq!(reaction.event.source, "webhook:test-hook");
    // No dispatcher attached in this test daemon — watch-only, so Observed — the point here is
    // only that the injected event reached `react()` at all, not what it decided.
    assert!(matches!(reaction.outcome, ReactionOutcome::Observed));

    handle.abort();
}
