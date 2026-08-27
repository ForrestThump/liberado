//! Survivor tests for `handlers/status.rs`: label polarity, the context-fill
//! arithmetic, and its zero/overflow boundaries.

use super::*;
use crate::context::StatusInfo;
use crate::result::CommandResult;
use crate::test_mock::SurvivorCtx;

fn base_status() -> StatusInfo {
    StatusInfo {
        running: true,
        vault_path: "/vault".into(),
        uptime_seconds: 3661,
        model_name: Some("test-model".into()),
        token_usage_total: Some(50),
        context_window: Some(100),
        dispatcher_attached: true,
        orchestrator_attached: false,
        reactions_seen: 7,
    }
}

#[test]
fn labels_render_both_polarities() {
    // Running + stopped.
    let mut ctx = SurvivorCtx::with_status(base_status());
    handle(&mut ctx);
    let info = ctx.last_message().to_string();
    assert!(info.contains("running"), "{info}");

    let mut stopped = base_status();
    stopped.running = false;
    let mut ctx = SurvivorCtx::with_status(stopped);
    handle(&mut ctx);
    assert!(
        ctx.last_message().contains("stopped"),
        "{}",
        ctx.last_message()
    );

    // Attached + detached, one of each in the same rendering.
    let mut mixed = base_status();
    mixed.dispatcher_attached = false;
    mixed.orchestrator_attached = true;
    let mut ctx = SurvivorCtx::with_status(mixed);
    handle(&mut ctx);
    let info = ctx.last_message().to_string();
    assert!(info.contains("detached"), "{info}");
    assert!(info.contains("attached"), "{info}");
}

/// A healthy window renders the real percentage.
#[test]
fn fill_shows_the_true_percentage() {
    let mut ctx = SurvivorCtx::with_status(base_status());
    handle(&mut ctx);
    assert!(
        ctx.last_message().contains("(50% context)"),
        "{}",
        ctx.last_message()
    );
}

/// A zero-width window cannot host a percentage: the guard is strict `w > 0`.
#[test]
fn zero_window_reports_placeholder() {
    let mut status = base_status();
    status.context_window = Some(0);
    let mut ctx = SurvivorCtx::with_status(status);
    handle(&mut ctx);
    assert!(
        ctx.last_message().contains("(-- context)"),
        "{}",
        ctx.last_message()
    );
}

/// A tiny window with heavy usage caps at the display ceiling rather than
/// printing four digits (or dividing by a width too small to count).
#[test]
fn oversized_ratio_caps_at_the_ceiling() {
    let mut status = base_status();
    status.token_usage_total = Some(1_000_000);
    status.context_window = Some(1);
    let mut ctx = SurvivorCtx::with_status(status);
    handle(&mut ctx);
    assert!(
        ctx.last_message().contains("(99% context)"),
        "{}",
        ctx.last_message()
    );
}

/// Without a daemon snapshot the command says so honestly and still reports
/// that status was shown.
#[test]
fn missing_status_prints_the_waiting_note() {
    let mut ctx = SurvivorCtx::new();
    let results = handle(&mut ctx);
    assert_eq!(results, vec![CommandResult::StatusShown]);
    assert!(
        ctx.last_message().contains("Not connected"),
        "{}",
        ctx.last_message()
    );
}
