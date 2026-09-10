//! Frozen contract for daemon-native pull-request review.
//!
//! Slices 0 and 1 define and validate this boundary, but do not execute a reviewer.

use serde::{Deserialize, Serialize};

use crate::ReviewWorkerConfig;
pub use crate::pr_review_cycle::review_key;
use crate::pr_review_cycle::tip_already_accepted;
pub use crate::pr_review_parse::{parse_codex_success, parse_opencode_success};

pub const REVIEW_SCHEMA_VERSION: &str = "liberado.pr-review.v1";
pub const POLICY_VERSION: &str = "daemon-pr-review-v1";
pub const MAX_PROMPT_BYTES: usize = 64 * 1024;
pub const MAX_DIFF_BYTES: usize = 512 * 1024;
pub const MAX_RAW_OUTPUT_BYTES: usize = 2 * 1024 * 1024;
pub const CODEX_USAGE_LIMIT: &str = "ERROR: You've hit your usage limit";
pub use crate::control_plane::OPENCODE_NAMED_REVIEW_MODEL;
const CODEX_USAGE_MARKERS: &[&str] = &[
    CODEX_USAGE_LIMIT,
    "You've hit your usage limit",
    "You've reached your weekly limit",
    "usage limit reached",
];
pub const REQUIRED_GITHUB_PERMISSIONS: &[&str] = &[
    "metadata:read",
    "contents:read",
    "pull_requests:read",
    "pull_requests:write",
    "checks:read",
    "statuses:read",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewResult {
    pub schema: String,
    pub reviewed_sha: String,
    pub summary: String,
    #[serde(default)]
    pub findings: Vec<ReviewFinding>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewFinding {
    pub id: String,
    pub severity: ReviewSeverity,
    pub blocker: bool,
    pub path: String,
    #[serde(default)]
    pub line: Option<u32>,
    pub explanation: String,
    #[serde(default)]
    pub check: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewSeverity {
    Low,
    Medium,
    High,
    Critical,
}

impl ReviewResult {
    pub fn parse_success(output: &str, expected_sha: &str) -> Result<Self, ReviewResultError> {
        let result: Self =
            serde_json::from_str(output).map_err(|_| ReviewResultError::Malformed)?;
        if result.schema != REVIEW_SCHEMA_VERSION || result.summary.trim().is_empty() {
            return Err(ReviewResultError::Malformed);
        }
        if !valid_full_sha(&result.reviewed_sha) || result.reviewed_sha != expected_sha {
            return Err(ReviewResultError::StaleSha);
        }
        if result.findings.iter().any(|finding| {
            finding.id.trim().is_empty()
                || finding.path.trim().is_empty()
                || finding.explanation.trim().is_empty()
        }) {
            return Err(ReviewResultError::Malformed);
        }
        Ok(result)
    }
}

pub fn valid_full_sha(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ReviewResultError {
    #[error("review result is malformed")]
    Malformed,
    #[error("review result SHA does not match the expected full SHA")]
    StaleSha,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerFailure {
    Exhausted,
    RateLimited,
    Auth,
    Permission,
    Timeout,
    ModelFailure,
}

/// Classify only stable, captured signals. Exit status alone never selects fallback.
pub fn classify_codex_failure(output: &str) -> WorkerFailure {
    if CODEX_USAGE_MARKERS
        .iter()
        .any(|marker| output.contains(marker))
    {
        WorkerFailure::Exhausted
    } else if output.contains("rate limit") || output.contains("429") {
        WorkerFailure::RateLimited
    } else if output.contains("authentication") || output.contains("Unauthorized") {
        WorkerFailure::Auth
    } else if output.contains("permission denied") || output.contains("Permission denied") {
        WorkerFailure::Permission
    } else if output.contains("timed out") || output.contains("timeout") {
        WorkerFailure::Timeout
    } else {
        WorkerFailure::ModelFailure
    }
}

/// OpenCode review failures use captured text only. Unknown quota wording stays a failure.
pub fn classify_opencode_failure(output: &str) -> WorkerFailure {
    classify_codex_failure(output)
}

/// Frozen Codex command. It is evidence for Slice 2; no Slice 0/1 caller starts it.
pub fn codex_review_args(base_sha: &str, head_sha: &str, schema_path: &str) -> Vec<String> {
    vec![
        "exec".into(),
        "review".into(),
        "--sandbox".into(),
        "read-only".into(),
        "--json".into(),
        "--output-schema".into(),
        schema_path.into(),
        "--base".into(),
        base_sha.into(),
        "--commit".into(),
        head_sha.into(),
    ]
}

pub fn grok_review_args(schema_path: &str) -> Vec<String> {
    vec![
        "review".into(),
        "--headless".into(),
        "--schema".into(),
        schema_path.into(),
    ]
}

pub fn antigravity_review_args(schema_path: &str) -> Vec<String> {
    vec![
        "--print".into(),
        "--output-format".into(),
        "stream-json".into(),
        "--schema".into(),
        schema_path.into(),
    ]
}

pub fn cursor_review_args() -> Vec<String> {
    vec!["--mode".into(), "ask".into(), "--print".into()]
}

/// Frozen OpenCode command. It does not pass `--auto`, so writes stay denied.
pub fn opencode_review_args(model: &str, schema_path: &str, prompt: &str) -> Vec<String> {
    vec![
        "run".into(),
        "--format".into(),
        "json".into(),
        "--model".into(),
        model.into(),
        "--dir".into(),
        ".".into(),
        format!(
            "{prompt} The required result schema is in {schema_path}. Return only that JSON object."
        ),
    ]
}

pub fn review_prompt(base_sha: &str, head_sha: &str) -> String {
    format!(
        "Review the changes from base commit {base_sha} to HEAD {head_sha}. Return only the \
         review result required by the supplied JSON schema. Set reviewed_sha to {head_sha}. \
         Do not modify files."
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequestSnapshot {
    pub repository: String,
    pub number: u64,
    pub head_sha: String,
    pub open: bool,
    pub draft: bool,
    pub fork_head: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewPolicy {
    pub controller: String,
    pub check_names: Vec<String>,
    pub shadow: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewCycle {
    pub armed_sha: Option<String>,
    pub accepted_sha: Option<String>,
    pub accepted_review_key: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckConclusion {
    Success,
    Pending,
    Failure,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShaChecks {
    pub sha: String,
    pub checks: Vec<(String, CheckConclusion)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WakeSignal {
    Poll,
    ReadyForReview { sha: String },
    ReviewRequested,
    Opened,
    Synchronize { old_sha: String },
    ConvertedToDraft,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObserverIntent {
    Arm { sha: String },
    ObserveTip { sha: String },
    ObserveCi { sha: String, passed: bool },
    ObserveDraft { sha: String },
    Eligible { sha: String },
    SynchronizeDraftAndNote { old_sha: String, new_sha: String },
    Close { sha: String },
}

/// Durable facts used to rebuild [`ReviewCycle`] after a restart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CycleFact {
    Armed { sha: String },
    Draft,
    Closed,
    Synchronized,
    Published { sha: String, review_key: String },
}

pub fn review_cycle(facts: impl IntoIterator<Item = CycleFact>) -> ReviewCycle {
    let mut cycle = ReviewCycle {
        armed_sha: None,
        accepted_sha: None,
        accepted_review_key: None,
    };
    for fact in facts {
        match fact {
            CycleFact::Armed { sha } => cycle.armed_sha = Some(sha),
            CycleFact::Draft | CycleFact::Closed | CycleFact::Synchronized => {
                cycle.armed_sha = None;
            }
            CycleFact::Published { sha, review_key } => {
                cycle.accepted_sha = Some(sha);
                cycle.accepted_review_key = Some(review_key);
            }
        }
    }
    cycle
}

/// Poll-derived wake. Webhook `review_requested` never appears here; it cannot arm.
pub fn poll_signal(
    pr: &PullRequestSnapshot,
    cycle: &ReviewCycle,
    prior_tip: Option<&str>,
    ready_for_current: bool,
) -> WakeSignal {
    if !pr.open {
        return WakeSignal::Closed;
    }
    if let Some(old_sha) = prior_tip.filter(|old| *old != pr.head_sha.as_str())
        && (cycle.armed_sha.as_deref() == Some(old_sha)
            || cycle.accepted_sha.as_deref() == Some(old_sha))
    {
        return WakeSignal::Synchronize {
            old_sha: old_sha.to_string(),
        };
    }
    if pr.draft {
        return WakeSignal::ConvertedToDraft;
    }
    if ready_for_current {
        return WakeSignal::ReadyForReview {
            sha: pr.head_sha.clone(),
        };
    }
    WakeSignal::Poll
}

/// Shared daemon/CLI observation: derive the poll signal, then apply [`observe`].
pub fn reconcile_snapshot(
    policy: &ReviewPolicy,
    pr: &PullRequestSnapshot,
    cycle: &ReviewCycle,
    checks: &ShaChecks,
    prior_tip: Option<&str>,
    ready_for_current: bool,
) -> Vec<ObserverIntent> {
    observe(
        policy,
        pr,
        cycle,
        checks,
        poll_signal(pr, cycle, prior_tip, ready_for_current),
    )
}

pub fn parse_check_conclusion(conclusion: Option<&str>, status: Option<&str>) -> CheckConclusion {
    let raw = conclusion.or(status).unwrap_or("");
    if raw.eq_ignore_ascii_case("success") {
        CheckConclusion::Success
    } else if matches!(
        raw.to_ascii_lowercase().as_str(),
        "pending" | "queued" | "in_progress" | "waiting" | ""
    ) {
        CheckConclusion::Pending
    } else {
        CheckConclusion::Failure
    }
}

pub fn collect_github_checks(value: &serde_json::Value) -> Vec<(String, CheckConclusion)> {
    let rows = value["check_runs"]
        .as_array()
        .or_else(|| value["statuses"].as_array());
    rows.into_iter()
        .flatten()
        .filter_map(|row| {
            let name = row["name"].as_str().or_else(|| row["context"].as_str())?;
            let conclusion = parse_check_conclusion(
                row["conclusion"].as_str(),
                row["status"].as_str().or_else(|| row["state"].as_str()),
            );
            Some((name.to_string(), conclusion))
        })
        .collect()
}

pub fn pull_request_snapshot(
    repository: &str,
    value: &serde_json::Value,
) -> Option<PullRequestSnapshot> {
    let head_repo = value
        .pointer("/head/repo/full_name")
        .and_then(|v| v.as_str());
    let base_repo = value
        .pointer("/base/repo/full_name")
        .and_then(|v| v.as_str());
    Some(PullRequestSnapshot {
        repository: repository.to_string(),
        number: value["number"].as_u64()?,
        head_sha: value.pointer("/head/sha")?.as_str()?.to_string(),
        open: value["state"].as_str() == Some("open"),
        draft: value["draft"].as_bool().unwrap_or(true),
        fork_head: head_repo.is_none() || base_repo.is_none() || head_repo != base_repo,
    })
}

pub fn ready_for_review_commit(value: &serde_json::Value) -> Option<&str> {
    (value["event"].as_str() == Some("ready_for_review"))
        .then(|| value["commit_id"].as_str())
        .flatten()
}

/// Disabled workers yield no argv. Slices 0/1 never spawn the returned command.
pub fn review_process_argv(worker: &ReviewWorkerConfig, schema_path: &str) -> Option<Vec<String>> {
    if !worker.enabled() {
        return None;
    }
    Some(match worker {
        ReviewWorkerConfig::GrokBuild { executable, .. } => {
            prepend_exe(executable, grok_review_args(schema_path))
        }
        ReviewWorkerConfig::Codex { executable, .. } => {
            prepend_exe(executable, codex_review_args("base", "head", schema_path))
        }
        ReviewWorkerConfig::Antigravity { executable, .. } => {
            prepend_exe(executable, antigravity_review_args(schema_path))
        }
        ReviewWorkerConfig::CursorLocal { executable, .. } => {
            prepend_exe(executable, cursor_review_args())
        }
        ReviewWorkerConfig::OpenCode {
            executable, model, ..
        } => prepend_exe(
            executable,
            opencode_review_args(model, schema_path, &review_prompt("base", "head")),
        ),
        ReviewWorkerConfig::OpenaiCompatible { .. } => return None,
    })
}

fn prepend_exe(executable: &str, args: Vec<String>) -> Vec<String> {
    let mut command = vec![executable.to_string()];
    command.extend(args);
    command
}

pub fn slice01_records(intent: &ObserverIntent) -> bool {
    !matches!(intent, ObserverIntent::Eligible { .. })
}

pub fn required_checks_pass(policy: &ReviewPolicy, head_sha: &str, observed: &ShaChecks) -> bool {
    observed.sha == head_sha
        && !policy.check_names.is_empty()
        && policy.check_names.iter().all(|required| {
            let matching: Vec<_> = observed
                .checks
                .iter()
                .filter(|(name, _)| name == required)
                .collect();
            !matching.is_empty()
                && matching
                    .iter()
                    .all(|(_, conclusion)| *conclusion == CheckConclusion::Success)
        })
}

/// Pure eligibility shared by daemon polling and CLI dry-run.
pub fn review_eligible(
    policy: &ReviewPolicy,
    pr: &PullRequestSnapshot,
    cycle: &ReviewCycle,
    checks: &ShaChecks,
) -> bool {
    pr.open
        && !pr.draft
        && !pr.fork_head
        && cycle.armed_sha.as_deref() == Some(pr.head_sha.as_str())
        && !tip_already_accepted(cycle, pr)
        && required_checks_pass(policy, &pr.head_sha, checks)
}

pub fn observe(
    policy: &ReviewPolicy,
    pr: &PullRequestSnapshot,
    cycle: &ReviewCycle,
    checks: &ShaChecks,
    signal: WakeSignal,
) -> Vec<ObserverIntent> {
    let mut next = cycle.clone();
    let mut intents = vec![ObserverIntent::ObserveTip {
        sha: pr.head_sha.clone(),
    }];
    match signal {
        WakeSignal::ReadyForReview { sha } if sha == pr.head_sha => {
            next.armed_sha = Some(sha.clone());
            intents.push(ObserverIntent::Arm { sha });
        }
        WakeSignal::Synchronize { old_sha }
            if cycle.armed_sha.as_deref() == Some(old_sha.as_str())
                || cycle.accepted_sha.as_deref() == Some(old_sha.as_str()) =>
        {
            intents.push(ObserverIntent::SynchronizeDraftAndNote {
                old_sha,
                new_sha: pr.head_sha.clone(),
            });
            return intents;
        }
        WakeSignal::ConvertedToDraft => {
            next.armed_sha = None;
            intents.push(ObserverIntent::ObserveDraft {
                sha: pr.head_sha.clone(),
            });
        }
        WakeSignal::Closed => {
            intents.push(ObserverIntent::Close {
                sha: pr.head_sha.clone(),
            });
            return intents;
        }
        _ => {}
    }
    let passed = required_checks_pass(policy, &pr.head_sha, checks);
    intents.push(ObserverIntent::ObserveCi {
        sha: pr.head_sha.clone(),
        passed,
    });
    if review_eligible(policy, pr, &next, checks) {
        intents.push(ObserverIntent::Eligible {
            sha: pr.head_sha.clone(),
        });
    }
    intents
}

pub fn controller_lease_required(policy: &ReviewPolicy) -> bool {
    !policy.shadow
}

/// SHA-pinned GitHub endpoints used by every observer. Callers add bounded page parameters.
pub fn check_endpoints(repository: &str, sha: &str) -> [String; 2] {
    [
        format!("/repos/{repository}/commits/{sha}/check-runs?filter=latest&per_page=100"),
        format!("/repos/{repository}/commits/{sha}/status?per_page=100"),
    ]
}

pub fn bounded_pages(max_pages: usize) -> impl Iterator<Item = usize> {
    1..=max_pages
}

#[cfg(test)]
#[path = "pr_review_tests.rs"]
mod tests;
