//! Live, interactive end-to-end smoke test for the full Telegram Approve/Reject/Revise loop.
//! Ignored by default — touches the real Telegram API and a real model, and waits for a human to
//! tap a button. Run explicitly:
//!
//! ```sh
//! TELEGRAM_BOT_TOKEN=... TELEGRAM_CHAT_ID=... DEEPSEEK_API_KEY=... \
//!     cargo test -p liberado-telegram-approvals --test live_smoke -- --ignored --nocapture
//! ```
//!
//! Wires a real `Daemon` + `Orchestrator` (over a throwaway temp vault, with a mock tool runtime
//! from `liberado-test-support` so approving does something real but harmless — no external side
//! effects) and a real `ApprovalBot`, writes one signed Pending proposal, sends the real buttoned
//! Telegram notification, then watches the daemon's reaction channel so whatever a human taps gets
//! printed here. How long to wait is `SMOKE_TEST_TIMEOUT_SECS` (default 300 = 5 minutes) — this is
//! a manually-run test tool, not daemon runtime behavior, so an env var rather than
//! `config.tuning` (which the test doesn't load a real `Config` for at all).

use std::sync::Arc;
use std::time::{Duration, Instant};

use liberado_common::{CapabilitySet, Proposal, ProposalSigner, ProposedAction, ToolCall};
use liberado_config_loader::TelegramApprovalsTuning;
use liberado_daemon::Daemon;
use liberado_notify::{Notifier, TelegramNotifier};
use liberado_orchestrator::Orchestrator;
use liberado_provider::{CompletionResponse, MockProvider};
use liberado_provider_deepseek::DeepSeekProvider;
use liberado_telegram_approvals::ApprovalBot;
use liberado_test_support::{InvocationRecordingFactory, InvocationRecordingRuntime};
use tokio::sync::mpsc::unbounded_channel;

/// Default wait for a human to tap a button, in seconds — overridable via `SMOKE_TEST_TIMEOUT_SECS`.
const DEFAULT_SMOKE_TEST_TIMEOUT_SECS: u64 = 300;

fn smoke_test_timeout() -> Duration {
    let secs = std::env::var("SMOKE_TEST_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_SMOKE_TEST_TIMEOUT_SECS);
    Duration::from_secs(secs)
}

#[tokio::test]
#[ignore = "hits the real Telegram API + a real model, and waits on a human tap — run manually \
            with TELEGRAM_BOT_TOKEN/TELEGRAM_CHAT_ID/DEEPSEEK_API_KEY set"]
async fn live_full_approval_loop_smoke_test() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault_path = dir.path().to_path_buf();
    std::fs::create_dir_all(vault_path.join("proposals")).unwrap();

    let signer = ProposalSigner::random();
    let notifier: Arc<dyn Notifier> =
        Arc::new(TelegramNotifier::from_env().expect("set TELEGRAM_BOT_TOKEN + TELEGRAM_CHAT_ID"));

    // A mock tool runtime: approving records a fake `tasks:create` call in-memory — real execution
    // wiring, zero external side effects. Mirrors `daemon::tests::daemon_executes_an_approved_proposal`.
    let runtime = InvocationRecordingRuntime::default();
    let invoked = runtime.invoked.clone();
    let orch = Orchestrator::new(
        Arc::new(MockProvider::with_script(
            "mock",
            Vec::<CompletionResponse>::new(),
        )),
        InvocationRecordingFactory { runtime },
        CapabilitySet::empty(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        vault_path.clone(),
        signer.clone(),
        "default",
    )
    .with_notifier(notifier.clone());

    let daemon = Daemon::open("smoke-test", &vault_path)
        .await
        .unwrap()
        .with_orchestrator(orch)
        .with_proposal_signer(signer.clone())
        .with_notifier(notifier.clone());

    let provider = Arc::new(DeepSeekProvider::from_env().expect("set DEEPSEEK_API_KEY"));
    let bot = ApprovalBot::from_env(
        daemon.vault().clone(),
        signer.clone(),
        provider,
        TelegramApprovalsTuning::default(),
    )
    .expect("set TELEGRAM_BOT_TOKEN + TELEGRAM_CHAT_ID");

    let (tx, mut rx) = unbounded_channel();
    tokio::spawn(daemon.run(tx));
    tokio::spawn(bot.run());

    // Let both watch/poll loops establish before writing the proposal.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let proposal = Proposal::pending(
        "smoke-test-1",
        "smoke-test-1",
        "smoke-test",
        ProposedAction::ToolCalls(vec![ToolCall {
            tool: "tasks:create".into(),
            args: serde_json::json!({ "summary": "Telegram approval smoke test" }),
        }]),
        "This is a harmless smoke test proposal — approving it only records a mock tool call, \
         nothing external happens. Try Approve, Reject, or Revise.",
    );
    let proposal = signer.sign(proposal);
    let rel_path = format!("proposals/{}.md", proposal.id);
    std::fs::write(vault_path.join(&rel_path), proposal.to_note()).unwrap();

    notifier
        .notify_proposal(
            &proposal.id,
            "\u{1F527} Smoke test proposal — tap a button below.",
        )
        .await
        .expect("failed to send the initial notification");

    let timeout = smoke_test_timeout();
    println!(
        "Smoke test proposal sent to Telegram — waiting up to {}s for a button tap...",
        timeout.as_secs()
    );

    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            println!("Timed out waiting for a reaction — no button was tapped in time.");
            break;
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Some(reaction)) => {
                println!("REACTION: {}", reaction.outcome.label());
            }
            Ok(None) => {
                println!("Reaction channel closed.");
                break;
            }
            Err(_) => {
                println!("Timed out waiting for a reaction — no button was tapped in time.");
                break;
            }
        }
    }

    println!(
        "Recorded mock tool invocations: {:?}",
        invoked
            .lock()
            .unwrap()
            .iter()
            .map(|i| &i.name)
            .collect::<Vec<_>>()
    );
}
