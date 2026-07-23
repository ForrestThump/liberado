//! Pure helpers shared across daemon lifecycle phases.

use liberado_common::{DEFAULT_POOL, Event, event_source};
use liberado_session::{DomainHint, GoalSpec, SessionOrigin, TerminalKind};

use crate::types::REACTION_DOMAIN;

/// The goal a reaction's hosted session records. `goal` is what the dispatcher was actually asked —
/// templated from the path for a vault change, the configured goal text for a cron or webhook — so
/// the session says what the reaction was *for*, not merely that one happened.
///
/// The event's `correlation_id` rides on `origin` with **no** parent conversation: nobody spawned a
/// cron from a chat, but it still belongs to a dispatch journal entry. That is the case
/// `SessionOrigin::from_correlation` exists for.
///
/// The schedule name iff `source` is a cron source (`"cron:{name}"`), else `None`.
///
/// The gate for cron result delivery: only a `cron:`-sourced reaction should have its summary pushed
/// to the human. A vault-watch reaction (`"turbovault-subscription"`, no `:name`) or a `delegate`d
/// subagent must never leak here, so the match is on the source *kind*, not a substring.
pub(crate) fn cron_schedule_name(source: &str) -> Option<&str> {
    match source.split_once(':') {
        Some((kind, name)) if kind == event_source::CRON => Some(name),
        _ => None,
    }
}

/// Render a cron result for delivery. On success it is just the brief under the schedule name; any
/// non-success terminal is tagged so a failed/exhausted run can never be mistaken for a real report
/// (the honest-status rule from `Disposition::terminal_summary`).
pub(crate) fn format_cron_delivery(
    schedule: &str,
    summary: &str,
    terminal: TerminalKind,
) -> String {
    if matches!(terminal, TerminalKind::Succeeded) {
        format!("🕒 {schedule}\n\n{summary}")
    } else {
        format!("🕒 {schedule} [{terminal:?}]\n\n{summary}")
    }
}

/// The `policy.toml` grant `component` whose capability ceiling gates a given pool — so an
/// "everywhere" grant lands where the blocked path actually reads its authority. The default pool's
/// ceiling is `capabilities_for("dispatcher")`; a named pool's is `capabilities_for(<pool name>)`
/// (see `liberado_bootstrap::configure_daemon`). A permission request stamps its owning pool onto
/// `Proposal.pool`, which is `Some(DEFAULT_POOL)` ("default") for the default pool.
pub(crate) fn grant_component_for_pool(pool: Option<&str>) -> &str {
    match pool {
        None | Some(DEFAULT_POOL) => "dispatcher",
        Some(name) => name,
    }
}

/// The `proposals/archive/` subdirectory a resolved note is filed under, keyed by its terminal
/// status — so the folder self-describes the outcome. Non-terminal statuses return `None` (they
/// have no archive home yet), which is what keeps `archive_terminal_proposal` a no-op for them.
pub(crate) fn archive_outcome_subdir(
    status: liberado_common::ProposalStatus,
) -> Option<&'static str> {
    use liberado_common::ProposalStatus;
    match status {
        ProposalStatus::Done => Some("approved"),
        ProposalStatus::Rejected => Some("rejected"),
        ProposalStatus::Expired => Some("expired"),
        ProposalStatus::Pending | ProposalStatus::Approved => None,
    }
}

/// `pool` is stamped into `payload` so the dispatch pack routes to the same pool the event named.
pub(crate) fn reaction_goal(event: &Event, goal: &str, pool: &str) -> GoalSpec {
    let profile = event
        .payload
        .data
        .get("profile")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    GoalSpec {
        id: None,
        description: goal.to_string(),
        success_criteria: Vec::new(),
        domain: DomainHint::from(REACTION_DOMAIN),
        max_turns: 0,
        max_idle_secs: None,
        origin: Some(SessionOrigin::from_correlation(&event.correlation_id)),
        profile,
        payload: serde_json::json!({
            "source": event.source,
            "event_type": event.event_type,
            "path": event.payload.path,
            "pool": pool,
        }),
    }
}

/// Turn a correlation id into a single safe path segment for a proposal filename. Correlation ids
/// carry `:` and `/` (e.g. `vault-change:inbox/x.md:abc`), neither valid in a Windows filename and
/// the latter a directory separator — collapse every non-alphanumeric run to a single `-`.
pub(crate) fn slugify(id: &str) -> String {
    let mut out = String::with_capacity(id.len());
    let mut last_dash = false;
    for ch in id.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}
