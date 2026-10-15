//! Open permission requests in the live `proposals/` directory.
//!
//! Decided notes stay in that directory until the daemon archives them, so the
//! scan keeps a note only while [`recorded_action`](liberado_common::recorded_action)
//! is `pending`. Archive subdirectories are not walked.

use std::path::Path;

use chrono::{DateTime, Utc};
use liberado_common::{PROPOSALS_DIR, Proposal, recorded_action};

use super::PermissionResolver;

impl PermissionResolver {
    /// Pending permission requests, oldest first. Ordinary proposals are omitted.
    pub async fn list_pending_permission_requests(&self) -> Vec<Proposal> {
        let dir = self.vault.root().join(PROPOSALS_DIR);
        let mut found = read_pending_notes(&dir, Utc::now()).await;
        found.sort_by_key(|proposal| proposal.created);
        found
    }
}

async fn read_pending_notes(dir: &Path, now: DateTime<Utc>) -> Vec<Proposal> {
    let mut found = Vec::new();
    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(entries) => entries,
        Err(_) => return found,
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        push_if_pending(&entry.path(), now, &mut found).await;
    }
    found
}

async fn push_if_pending(path: &Path, now: DateTime<Utc>, found: &mut Vec<Proposal>) {
    if path.extension().and_then(|ext| ext.to_str()) != Some("md") {
        return;
    }
    let Ok(content) = tokio::fs::read_to_string(path).await else {
        return;
    };
    let Ok(proposal) = Proposal::from_note(&content) else {
        return;
    };
    if is_open_permission(&proposal, now) {
        found.push(proposal);
    }
}

fn is_open_permission(proposal: &Proposal, now: DateTime<Utc>) -> bool {
    proposal.requested_grant.is_some() && recorded_action(proposal, now) == "pending"
}

#[cfg(test)]
#[path = "pending_list_tests.rs"]
mod tests;
