//! Slice 4a routing: one command, one run per enabled worker, typed fallback only.

use std::collections::BTreeMap;

use crate::ReviewWorkerConfig;
use crate::TaskEventKind;
use crate::TaskLedger;
use crate::pr_review_admission::review_run_in_flight;

pub fn review_run_id(command_id: &str, worker_id: &str) -> String {
    format!("{command_id}:{worker_id}")
}

pub fn allows_fallback(reason: &str) -> bool {
    matches!(reason, "exhausted" | "rate_limited")
}

pub fn next_review_worker<'a>(
    ledger: &TaskLedger,
    command_id: &str,
    harness_order: &'a [String],
    workers: &'a BTreeMap<String, ReviewWorkerConfig>,
) -> Option<(&'a str, &'a ReviewWorkerConfig)> {
    if review_run_in_flight(ledger) || command_is_closed(ledger, command_id) {
        return None;
    }
    if command_was_issued(ledger, command_id) && !last_attempt_allows_fallback(ledger, command_id) {
        return None;
    }
    let attempted = attempted_workers(ledger, command_id);
    harness_order.iter().find_map(|id| {
        let worker = workers.get(id)?;
        if !review_adapter_supported(worker) || !worker.enabled() || attempted.contains(id) {
            return None;
        }
        Some((id.as_str(), worker))
    })
}

fn review_adapter_supported(worker: &ReviewWorkerConfig) -> bool {
    matches!(
        worker,
        ReviewWorkerConfig::Codex { .. } | ReviewWorkerConfig::OpenCode { .. }
    )
}

fn command_was_issued(ledger: &TaskLedger, command_id: &str) -> bool {
    ledger.events().iter().any(|event| {
        matches!(
            &event.payload,
            TaskEventKind::ReviewCommandIssued { command_id: id, .. } if id == command_id
        )
    })
}

fn command_is_closed(ledger: &TaskLedger, command_id: &str) -> bool {
    ledger.events().iter().any(|event| match &event.payload {
        TaskEventKind::ReviewRunFinished { run_id, .. }
        | TaskEventKind::ReviewStale { run_id, .. }
            if run_belongs(command_id, run_id) =>
        {
            true
        }
        TaskEventKind::ReviewWorkerUnavailable { run_id, reason, .. }
            if run_belongs(command_id, run_id) && !allows_fallback(reason) =>
        {
            true
        }
        _ => false,
    })
}

fn last_attempt_allows_fallback(ledger: &TaskLedger, command_id: &str) -> bool {
    ledger
        .events()
        .iter()
        .rev()
        .find_map(|event| match &event.payload {
            TaskEventKind::ReviewWorkerUnavailable { run_id, reason, .. }
                if run_belongs(command_id, run_id) =>
            {
                Some(allows_fallback(reason))
            }
            TaskEventKind::ReviewRunFinished { run_id, .. }
            | TaskEventKind::ReviewStale { run_id, .. }
                if run_belongs(command_id, run_id) =>
            {
                Some(false)
            }
            _ => None,
        })
        .unwrap_or(false)
}

fn attempted_workers(ledger: &TaskLedger, command_id: &str) -> Vec<String> {
    ledger
        .events()
        .iter()
        .filter_map(|event| match &event.payload {
            TaskEventKind::ReviewRunStarted {
                command_id: issued,
                worker_id,
                ..
            } if issued == command_id => Some(worker_id.clone()),
            TaskEventKind::ReviewWorkerUnavailable {
                run_id, worker_id, ..
            }
            | TaskEventKind::ReviewRunFinished {
                run_id, worker_id, ..
            } if run_belongs(command_id, run_id) => Some(worker_id.clone()),
            _ => None,
        })
        .collect()
}

fn run_belongs(command_id: &str, run_id: &str) -> bool {
    run_id == command_id || run_id.starts_with(&format!("{command_id}:"))
}

#[cfg(test)]
#[path = "pr_review_route_tests.rs"]
mod tests;
