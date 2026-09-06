//! Frozen contract for daemon-native pull-request review.
//!
//! Slices 0 and 1 define and validate this boundary, but do not execute a reviewer.

use serde::{Deserialize, Serialize};

pub const REVIEW_SCHEMA_VERSION: &str = "liberado.pr-review.v1";
pub const POLICY_VERSION: &str = "daemon-pr-review-v1";
pub const MAX_PROMPT_BYTES: usize = 64 * 1024;
pub const MAX_DIFF_BYTES: usize = 512 * 1024;
pub const MAX_RAW_OUTPUT_BYTES: usize = 2 * 1024 * 1024;
pub const CODEX_USAGE_LIMIT: &str = "ERROR: You've hit your usage limit";
pub const REQUIRED_GITHUB_PERMISSIONS: &[&str] = &[
    "metadata:read",
    "contents:read",
    "pull_requests:read",
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
pub fn classify_codex_failure(stderr: &str) -> WorkerFailure {
    if stderr
        .lines()
        .any(|line| line.starts_with(CODEX_USAGE_LIMIT))
    {
        WorkerFailure::Exhausted
    } else if stderr.contains("rate limit") || stderr.contains("429") {
        WorkerFailure::RateLimited
    } else if stderr.contains("authentication") || stderr.contains("Unauthorized") {
        WorkerFailure::Auth
    } else if stderr.contains("permission denied") || stderr.contains("Permission denied") {
        WorkerFailure::Permission
    } else if stderr.contains("timed out") || stderr.contains("timeout") {
        WorkerFailure::Timeout
    } else {
        WorkerFailure::ModelFailure
    }
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
    Eligible { sha: String },
    SynchronizeDraftAndNote { old_sha: String, new_sha: String },
    Close { sha: String },
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
        && cycle.accepted_sha.as_deref() != Some(pr.head_sha.as_str())
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
        WakeSignal::ConvertedToDraft => next.armed_sha = None,
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
mod tests {
    use super::*;

    fn valid(sha: &str) -> String {
        serde_json::json!({"schema":REVIEW_SCHEMA_VERSION,"reviewed_sha":sha,"summary":"clean","findings":[]}).to_string()
    }

    #[test]
    fn success_requires_exact_full_sha_and_valid_schema() {
        let sha = "a".repeat(40);
        assert!(ReviewResult::parse_success(&valid(&sha), &sha).is_ok());
        assert_eq!(
            ReviewResult::parse_success(&valid(&"b".repeat(40)), &sha),
            Err(ReviewResultError::StaleSha)
        );
        assert_eq!(
            ReviewResult::parse_success("{}", &sha),
            Err(ReviewResultError::Malformed)
        );
        assert_eq!(
            ReviewResult::parse_success(&valid("abc"), "abc"),
            Err(ReviewResultError::StaleSha)
        );
    }

    #[test]
    fn codex_failure_fixtures_are_distinct_and_402_is_not_exhaustion() {
        assert_eq!(
            classify_codex_failure("ERROR: You've hit your usage limit. Try again at 1:00 PM"),
            WorkerFailure::Exhausted
        );
        assert_eq!(
            classify_codex_failure("HTTP 429: rate limit"),
            WorkerFailure::RateLimited
        );
        assert_eq!(
            classify_codex_failure("Unauthorized: authentication failed"),
            WorkerFailure::Auth
        );
        assert_eq!(
            classify_codex_failure("Permission denied by sandbox"),
            WorkerFailure::Permission
        );
        assert_eq!(
            classify_codex_failure("operation timed out"),
            WorkerFailure::Timeout
        );
        assert_eq!(
            classify_codex_failure("HTTP 402 Payment Required"),
            WorkerFailure::ModelFailure
        );
        assert_eq!(
            classify_codex_failure("model returned an error"),
            WorkerFailure::ModelFailure
        );
    }

    #[test]
    fn codex_command_is_read_only_and_sha_pinned() {
        let args = codex_review_args("base", "head", "schema.json");
        assert_eq!(
            args,
            [
                "exec",
                "review",
                "--sandbox",
                "read-only",
                "--json",
                "--output-schema",
                "schema.json",
                "--base",
                "base",
                "--commit",
                "head"
            ]
        );
    }

    #[test]
    fn adapter_commands_are_frozen_without_execution() {
        assert_eq!(
            grok_review_args("schema"),
            ["review", "--headless", "--schema", "schema"]
        );
        assert_eq!(
            antigravity_review_args("schema"),
            [
                "--print",
                "--output-format",
                "stream-json",
                "--schema",
                "schema"
            ]
        );
        assert_eq!(cursor_review_args(), ["--mode", "ask", "--print"]);
    }

    fn policy() -> ReviewPolicy {
        ReviewPolicy {
            controller: "liberado-shepherd".into(),
            check_names: vec!["CI".into()],
            shadow: false,
        }
    }
    fn pr(repo: &str, sha: &str) -> PullRequestSnapshot {
        PullRequestSnapshot {
            repository: repo.into(),
            number: 1,
            head_sha: sha.into(),
            open: true,
            draft: false,
            fork_head: false,
        }
    }
    fn checks(sha: &str, conclusion: CheckConclusion) -> ShaChecks {
        ShaChecks {
            sha: sha.into(),
            checks: vec![("CI".into(), conclusion)],
        }
    }

    #[test]
    fn eligibility_is_sha_exact_success_only_and_rejects_forks() {
        let a = "a".repeat(40);
        let b = "b".repeat(40);
        let cycle = ReviewCycle {
            armed_sha: Some(a.clone()),
            accepted_sha: None,
        };
        assert!(review_eligible(
            &policy(),
            &pr("one/repo", &a),
            &cycle,
            &checks(&a, CheckConclusion::Success)
        ));
        assert!(!review_eligible(
            &policy(),
            &pr("one/repo", &a),
            &cycle,
            &checks(&b, CheckConclusion::Success)
        ));
        assert!(!review_eligible(
            &policy(),
            &pr("one/repo", &a),
            &cycle,
            &checks(&a, CheckConclusion::Pending)
        ));
        let mut fork = pr("one/repo", &a);
        fork.fork_head = true;
        assert!(!review_eligible(
            &policy(),
            &fork,
            &cycle,
            &checks(&a, CheckConclusion::Success)
        ));
    }

    #[test]
    fn wake_rules_do_not_infer_an_arm() {
        let sha = "a".repeat(40);
        let empty = ReviewCycle {
            armed_sha: None,
            accepted_sha: None,
        };
        for signal in [
            WakeSignal::Poll,
            WakeSignal::ReviewRequested,
            WakeSignal::Opened,
        ] {
            assert!(
                !observe(
                    &policy(),
                    &pr("one/repo", &sha),
                    &empty,
                    &checks(&sha, CheckConclusion::Success),
                    signal
                )
                .iter()
                .any(|intent| matches!(
                    intent,
                    ObserverIntent::Arm { .. } | ObserverIntent::Eligible { .. }
                ))
            );
        }
    }

    #[test]
    fn synchronize_is_one_draft_note_intent_and_never_review() {
        let old = "a".repeat(40);
        let new = "b".repeat(40);
        let cycle = ReviewCycle {
            armed_sha: Some(old.clone()),
            accepted_sha: None,
        };
        let intents = observe(
            &policy(),
            &pr("one/repo", &new),
            &cycle,
            &checks(&new, CheckConclusion::Success),
            WakeSignal::Synchronize { old_sha: old },
        );
        assert_eq!(
            intents
                .iter()
                .filter(|i| matches!(i, ObserverIntent::SynchronizeDraftAndNote { .. }))
                .count(),
            1
        );
        assert!(
            !intents
                .iter()
                .any(|i| matches!(i, ObserverIntent::Eligible { .. }))
        );
    }

    #[test]
    fn repository_identity_and_shadow_lease_are_explicit() {
        assert_ne!(
            pr("one/repo", &"a".repeat(40)),
            pr("two/repo", &"a".repeat(40))
        );
        let mut p = policy();
        p.shadow = true;
        assert!(!controller_lease_required(&p));
    }

    #[test]
    fn api_paths_are_sha_exact_and_page_iteration_is_bounded() {
        let paths = check_endpoints("owner/repo", "abc");
        assert_eq!(
            paths[0],
            "/repos/owner/repo/commits/abc/check-runs?filter=latest&per_page=100"
        );
        assert_eq!(
            paths[1],
            "/repos/owner/repo/commits/abc/status?per_page=100"
        );
        assert_eq!(bounded_pages(3).collect::<Vec<_>>(), [1, 2, 3]);
    }
}
