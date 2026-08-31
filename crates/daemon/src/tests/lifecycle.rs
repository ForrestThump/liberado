//! Tests for daemon background tasks, proposal expiry reaper, and persistent grants.

use super::test_fixtures::*;
use chrono::{Duration as ChronoDuration, Utc};
use liberado_common::{Proposal, ProposalSigner, ProposalStatus, ProposedAction, ToolCall};
use std::path::Path;
use std::time::Duration as StdDuration;

#[tokio::test]
async fn concurrent_park_and_cancel_do_not_deadlock() {
    use liberado_session::{
        DomainHint, DomainPackRunner, GoalResult, GoalSessionHub, GoalSessionStore, GoalSpec,
        InputChannel, PackContext, PackError, SessionEvent, SessionGrant, SessionStatus,
    };
    use std::sync::Arc;

    struct ConcurrentSpyPack {
        pub cancelled: Arc<std::sync::atomic::AtomicBool>,
    }
    #[async_trait::async_trait]
    impl DomainPackRunner for ConcurrentSpyPack {
        fn domain_id(&self) -> &str {
            "life"
        }
        async fn run(
            &self,
            _id: &str,
            _goal: &GoalSpec,
            _ctx: &PackContext<'_>,
            _events: tokio::sync::mpsc::Sender<SessionEvent>,
            _inputs: InputChannel,
            mut cancel: tokio::sync::watch::Receiver<bool>,
        ) -> Result<GoalResult, PackError> {
            loop {
                tokio::select! {
                    _ = cancel.changed() => {
                        self.cancelled.store(true, std::sync::atomic::Ordering::SeqCst);
                        return Err(PackError::Cancelled);
                    }
                    _ = tokio::time::sleep(StdDuration::from_millis(10)) => {}
                }
            }
        }
    }

    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let pack = Arc::new(ConcurrentSpyPack {
        cancelled: cancelled.clone(),
    });

    let mut hub = GoalSessionHub::new(GoalSessionStore::new());
    hub.register_pack(pack);
    let hub = Arc::new(hub);

    let session_id = hub
        .start_with_grant(
            GoalSpec {
                id: None,
                description: "concurrent test".into(),
                success_criteria: vec![],
                domain: DomainHint::Life,
                max_turns: 0,
                max_idle_secs: None,
                origin: None,
                profile: None,
                payload: serde_json::json!({}),
            },
            SessionGrant {
                capabilities: liberado_common::CapabilitySet::from_iter([
                    liberado_common::Capability::AskHuman,
                ]),
                ..Default::default()
            },
        )
        .await
        .expect("start session");

    // Wait for Running via snapshot loop
    for _ in 0..100 {
        if let Some(snap) = hub.snapshot(&session_id).await
            && snap.session.status == SessionStatus::Running
        {
            break;
        }
        tokio::time::sleep(StdDuration::from_millis(10)).await;
    }

    // Spawn concurrent snapshot + cancel tasks
    let hub_cancel = hub.clone();
    let hub_poll = hub.clone();
    let sid_kill = session_id.clone();
    let sid_poll = session_id.clone();

    let cancel_task = tokio::spawn(async move {
        tokio::time::sleep(StdDuration::from_millis(50)).await;
        let _ = hub_cancel.cancel(&sid_kill).await;
    });
    let poll_task = tokio::spawn(async move {
        for _ in 0..100 {
            if let Some(snap) = hub_poll.snapshot(&sid_poll).await
                && snap.session.status.is_terminal()
            {
                return;
            }
            tokio::time::sleep(StdDuration::from_millis(20)).await;
        }
    });

    tokio::time::timeout(StdDuration::from_secs(5), async {
        let _ = tokio::join!(cancel_task, poll_task);
    })
    .await
    .expect("concurrent snapshot + cancel must not deadlock");

    // Ground truth: session reached terminal
    let snap = hub.snapshot(&session_id).await.expect("snapshot");
    assert!(
        snap.session.status.is_terminal(),
        "session must be terminal after concurrent park/cancel, got {:?}",
        snap.session.status
    );
    assert_eq!(
        snap.session.status,
        SessionStatus::Cancelled,
        "session must be Cancelled after concurrent cancel"
    );
    assert!(
        cancelled.load(std::sync::atomic::Ordering::SeqCst),
        "pack must have seen cancellation signal"
    );
    // State-machine invariants must hold after terminal.
    liberado_session::check_session_invariants(&snap.session)
        .expect("session invariants violated after concurrent cancel");
}

#[tokio::test]
async fn proposal_reap_loop_archives_expired_approved_proposal() {
    let (daemon, _dir) = temp_daemon().await;
    let vault = daemon.vault.clone();
    let root = vault.root().to_path_buf();
    std::fs::create_dir_all(root.join("proposals")).unwrap();

    let proposal = Proposal::pending(
        "reap-loop:1",
        "reap-loop:1",
        "test",
        ProposedAction::ToolCalls(vec![ToolCall {
            tool: "tasks:create".into(),
            args: serde_json::json!({}),
        }]),
        "expired approved proposal",
    );
    let mut signed = ProposalSigner::random().sign(proposal);
    signed.set_status(ProposalStatus::Approved);
    let mut proposal = signed.into_proposal();
    proposal.expires = Some(Utc::now() - ChronoDuration::hours(2));
    std::fs::write(root.join("proposals/reap-loop-1.md"), proposal.to_note()).unwrap();

    let loop_vault = vault.clone();
    let handle = tokio::spawn(async move {
        crate::proposals::proposal_reap_loop(loop_vault, StdDuration::from_millis(50)).await;
    });
    // The loop skips the first interval fire, then waits one more tick before the first
    // sweep. Under a full Windows `cargo test` binary that 50ms tick is often starved;
    // 300ms failed `test (windows-latest)`. Match the 2s wait
    // `spawn_reaper_starts_the_expiry_reaper` already uses.
    tokio::time::sleep(StdDuration::from_millis(2000)).await;
    handle.abort();

    let archived = vault
        .read(Path::new("proposals/archive/expired/reap-loop-1.md"))
        .await;
    assert!(
        archived.is_ok(),
        "proposal_reap_loop must archive an expired approved proposal"
    );
    assert_eq!(
        Proposal::from_note(&archived.unwrap()).unwrap().status,
        ProposalStatus::Expired
    );
}

#[tokio::test]
async fn reap_expired_proposals_tolerates_missing_dir_and_non_dir() {
    let (daemon, _dir) = temp_daemon().await;
    let vault = daemon.vault.clone();
    let root = vault.root().to_path_buf();

    // Case 1: ensure no `proposals/` dir -> the NotFound arm must return Ok.
    let proposals_dir = root.join("proposals");
    if proposals_dir.exists() {
        std::fs::remove_dir_all(&proposals_dir).unwrap();
    }
    let result = crate::proposals::reap_expired_proposals(&vault).await;
    assert!(
        result.is_ok(),
        "reaping with no proposals dir must be a no-op Ok, got {:?}",
        result.err()
    );

    // Case 2: `proposals` is a *file* (not a directory) -> the listing error is not NotFound.
    // Original propagates it as Err; a mutant that always takes the Ok arm would swallow it.
    std::fs::write(&proposals_dir, "").unwrap();
    let result = crate::proposals::reap_expired_proposals(&vault).await;
    assert!(
        result.is_err(),
        "a non-NotFound listing error must propagate as Err, not be swallowed"
    );
}

#[tokio::test]
async fn spawn_reaper_starts_the_expiry_reaper() {
    let (base, _dir) = temp_daemon().await;
    let mut daemon = base.with_proposal_reap_interval(1);
    let vault = daemon.vault.clone();
    let root = vault.root().to_path_buf();
    std::fs::create_dir_all(root.join("proposals")).unwrap();

    let proposal = Proposal::pending(
        "spawn-reap:1",
        "spawn-reap:1",
        "test",
        ProposedAction::ToolCalls(vec![ToolCall {
            tool: "tasks:create".into(),
            args: serde_json::json!({}),
        }]),
        "expired approved proposal",
    );
    let mut signed = ProposalSigner::random().sign(proposal);
    signed.set_status(ProposalStatus::Approved);
    let mut proposal = signed.into_proposal();
    proposal.expires = Some(Utc::now() - ChronoDuration::hours(2));
    std::fs::write(root.join("proposals/spawn-reap-1.md"), proposal.to_note()).unwrap();

    daemon.spawn_reaper();
    tokio::time::sleep(StdDuration::from_millis(2000)).await;

    let archived = vault
        .read(Path::new("proposals/archive/expired/spawn-reap-1.md"))
        .await;
    assert!(
        archived.is_ok(),
        "spawn_reaper must launch the reaper that archives expired proposals"
    );
    assert_eq!(
        Proposal::from_note(&archived.unwrap()).unwrap().status,
        ProposalStatus::Expired
    );
}

#[tokio::test]
async fn persist_everywhere_grant_writes_to_overlay() {
    use liberado_common::{Capability, Zone};
    use std::env;

    let data_dir = tempfile::TempDir::new().unwrap();
    let prev = env::var("LIBERADO_DATA_DIR").ok();
    unsafe {
        env::set_var("LIBERADO_DATA_DIR", data_dir.path());
    }
    struct Restore(Option<String>);
    impl Drop for Restore {
        fn drop(&mut self) {
            unsafe {
                match &self.0 {
                    Some(v) => env::set_var("LIBERADO_DATA_DIR", v),
                    None => env::remove_var("LIBERADO_DATA_DIR"),
                }
            }
        }
    }
    let _restore = Restore(prev);

    let capability = Capability::Write(Zone::vault("tasks"));
    crate::proposals::persist_everywhere_grant("dispatcher", &capability);

    let overlay_path = data_dir.path().join("grants.overlay.toml");
    assert!(
        overlay_path.exists(),
        "persist_everywhere_grant must write the machine-owned overlay"
    );
    let contents = std::fs::read_to_string(&overlay_path).unwrap();
    assert!(
        contents.contains("tasks") && contents.contains("dispatcher"),
        "overlay must record the persisted grant"
    );
}
