//! Review-cycle identity helpers.

use crate::pr_review::{POLICY_VERSION, PullRequestSnapshot, ReviewCycle};

pub fn review_key(repository: &str, pr_number: u64, sha: &str, policy_version: &str) -> String {
    format!("{repository}#{pr_number}@{sha}:{policy_version}")
}

pub(crate) fn tip_already_accepted(cycle: &ReviewCycle, pr: &PullRequestSnapshot) -> bool {
    let key = review_key(&pr.repository, pr.number, &pr.head_sha, POLICY_VERSION);
    cycle.accepted_review_key.as_deref() == Some(key.as_str())
        || (cycle.accepted_review_key.is_none()
            && cycle.accepted_sha.as_deref() == Some(pr.head_sha.as_str()))
}
