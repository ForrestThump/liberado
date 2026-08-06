//! Domain-agnostic verifier DTOs (coding pack is the first consumer).
//!
//! See `docs/architecture/verifiers.md`. Types intentionally avoid git/cargo so they can graduate
//! to `liberado-common` when a second domain needs them.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::CoderCommandConfig;

/// One configured check in a goal contract / run config.
///
/// Live intake models often omit `id` or emit a single string where we want a list — fields below
/// default generously so freeze validation (not JSON parse) is the hard gate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum VerifierSpec {
    PathsExist {
        #[serde(default = "default_id_paths_exist")]
        id: String,
        #[serde(default, deserialize_with = "crate::intake::deserialize_string_or_vec")]
        paths: Vec<String>,
    },
    PathsAbsent {
        #[serde(default = "default_id_paths_absent")]
        id: String,
        #[serde(default, deserialize_with = "crate::intake::deserialize_string_or_vec")]
        paths: Vec<String>,
    },
    ContentContains {
        #[serde(default = "default_id_content_contains")]
        id: String,
        #[serde(default)]
        path: String,
        #[serde(default, deserialize_with = "crate::intake::deserialize_string_or_vec")]
        must_include: Vec<String>,
    },
    Command {
        #[serde(default = "default_id_command")]
        id: String,
        /// Default empty so incomplete live-model JSON still deserializes; sanitize drops empties.
        #[serde(default)]
        program: String,
        #[serde(default, deserialize_with = "crate::intake::deserialize_string_or_vec")]
        args: Vec<String>,
        #[serde(default)]
        env: BTreeMap<String, String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_secs: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_max_bytes: Option<usize>,
        /// When true, sandbox/network policy may allow outbound network.
        #[serde(default)]
        network: bool,
    },
    /// Coding-pack convenience: require non-empty `git status --porcelain`.
    GitNonemptyDiff {
        #[serde(default = "default_id_git_diff")]
        id: String,
    },
}

fn default_id_paths_exist() -> String {
    "paths_exist".into()
}
fn default_id_paths_absent() -> String {
    "paths_absent".into()
}
fn default_id_content_contains() -> String {
    "content_contains".into()
}
fn default_id_command() -> String {
    "command".into()
}
fn default_id_git_diff() -> String {
    "has_diff".into()
}

impl VerifierSpec {
    pub fn id(&self) -> &str {
        match self {
            Self::PathsExist { id, .. }
            | Self::PathsAbsent { id, .. }
            | Self::ContentContains { id, .. }
            | Self::Command { id, .. }
            | Self::GitNonemptyDiff { id, .. } => id,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::PathsExist { .. } => "paths_exist",
            Self::PathsAbsent { .. } => "paths_absent",
            Self::ContentContains { .. } => "content_contains",
            Self::Command { .. } => "command",
            Self::GitNonemptyDiff { .. } => "git_nonempty_diff",
        }
    }

    /// Build a command verifier from the legacy single validation_command field.
    pub fn from_command_config(id: impl Into<String>, cmd: &CoderCommandConfig) -> Self {
        Self::Command {
            id: id.into(),
            program: cmd.program.clone(),
            args: cmd.args.clone(),
            env: cmd.env.clone(),
            timeout_secs: cmd.timeout_secs,
            output_max_bytes: cmd.output_max_bytes,
            network: false,
        }
    }
}

/// One **gate's** verdict — not a session outcome, and deliberately not merged with one (V1).
///
/// `Error` is the variant that carries the weight: the check itself broke (the command would not
/// run, the path could not be read) as opposed to `Fail`, where the check ran fine and the *code* is
/// wrong. Collapsing those into a single "failed" is how a broken verifier gets mistaken for broken
/// work, and the loop then tries to "fix" code that was never at fault.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerdictStatus {
    Pass,
    Fail,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingKind {
    MissingPath,
    ContentMismatch,
    CommandFailed,
    CommandTimeout,
    PolicyDenied,
    UnexpectedChange,
    EmptyDiff,
    Custom,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    pub check_id: String,
    pub kind: FindingKind,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Verdict {
    pub status: VerdictStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    pub summary: String,
    #[serde(default)]
    pub findings: Vec<Finding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_excerpt: Option<String>,
}

impl Verdict {
    pub fn pass(summary: impl Into<String>) -> Self {
        Self {
            status: VerdictStatus::Pass,
            signature: None,
            summary: summary.into(),
            findings: Vec::new(),
            log_excerpt: None,
        }
    }

    pub fn fail(
        summary: impl Into<String>,
        findings: Vec<Finding>,
        log_excerpt: Option<String>,
    ) -> Self {
        let signature = Some(signature_for(&findings, log_excerpt.as_deref()));
        Self {
            status: VerdictStatus::Fail,
            signature,
            summary: summary.into(),
            findings,
            log_excerpt,
        }
    }

    pub fn error(summary: impl Into<String>) -> Self {
        let summary = summary.into();
        Self {
            status: VerdictStatus::Error,
            signature: Some(format!("error:{summary}")),
            summary,
            findings: Vec::new(),
            log_excerpt: None,
        }
    }

    pub fn is_pass(&self) -> bool {
        self.status == VerdictStatus::Pass
    }
}

fn signature_for(findings: &[Finding], log: Option<&str>) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    for f in findings {
        h.update(f.check_id.as_bytes());
        h.update(format!("{:?}", f.kind).as_bytes());
        h.update(f.message.as_bytes());
    }
    if let Some(log) = log {
        h.update(log.chars().take(200).collect::<String>().as_bytes());
    }
    format!("{:x}", h.finalize())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PipelineResult {
    pub overall: VerdictStatus,
    pub results: Vec<NamedVerdict>,
    pub combined_findings: Vec<Finding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub combined_signature: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NamedVerdict {
    pub id: String,
    pub kind: String,
    pub verdict: Verdict,
}

impl PipelineResult {
    pub fn is_pass(&self) -> bool {
        self.overall == VerdictStatus::Pass
    }

    /// Agent-facing repair feedback (also works as prior_feedback lines).
    pub fn repair_feedback(&self) -> String {
        let mut lines = vec!["Completeness/validation failed:".to_string()];
        if self.combined_findings.is_empty() {
            for r in &self.results {
                if !r.verdict.is_pass() {
                    lines.push(format!("- {}: {}", r.id, r.verdict.summary));
                }
            }
        } else {
            for f in &self.combined_findings {
                lines.push(format!("- {}: {}", f.check_id, f.message));
            }
        }
        lines.push("Fix these before claiming success.".to_string());
        lines.join("\n")
    }
}

/// Pipeline execution policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PipelinePolicy {
    #[serde(default = "default_true")]
    pub fail_fast: bool,
    #[serde(default = "default_true")]
    pub errors_are_failures: bool,
}

fn default_true() -> bool {
    true
}

impl Default for PipelinePolicy {
    fn default() -> Self {
        Self {
            fail_fast: true,
            errors_are_failures: true,
        }
    }
}

/// Resolve the effective verifier list: explicit `verifiers`, else legacy single command.
pub fn resolve_verifier_specs(
    verifiers: &[VerifierSpec],
    validation_command: Option<&CoderCommandConfig>,
) -> Vec<VerifierSpec> {
    if !verifiers.is_empty() {
        return verifiers.to_vec();
    }
    if let Some(cmd) = validation_command {
        return vec![VerifierSpec::from_command_config("validate", cmd)];
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifier_defaults_missing_id_and_string_paths() {
        let raw = r#"{
            "type": "paths_exist",
            "paths": "src/main.rs"
        }"#;
        let v: VerifierSpec = serde_json::from_str(raw).unwrap();
        assert_eq!(v.id(), "paths_exist");
        match v {
            VerifierSpec::PathsExist { paths, .. } => {
                assert_eq!(paths, vec!["src/main.rs".to_string()]);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn resolve_falls_back_to_validation_command() {
        let mut cmd = CoderCommandConfig::new("cargo");
        cmd.args = vec!["test".into()];
        let specs = resolve_verifier_specs(&[], Some(&cmd));
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].id(), "validate");
        assert_eq!(specs[0].kind(), "command");
    }

    #[test]
    fn repair_feedback_lists_findings() {
        let pipeline = PipelineResult {
            overall: VerdictStatus::Fail,
            results: Vec::new(),
            combined_findings: vec![Finding {
                check_id: "paths".into(),
                kind: FindingKind::MissingPath,
                message: "missing src/main.rs".into(),
                detail: None,
            }],
            combined_signature: Some("abc".into()),
        };
        let fb = pipeline.repair_feedback();
        assert!(fb.contains("src/main.rs"));
        assert!(fb.contains("Fix these"));
    }

    // ── Verdict constructors ────────────────────────────────────────────────

    #[test]
    fn verdict_pass_sets_status_and_summary() {
        let v = Verdict::pass("all good");
        assert_eq!(v.status, VerdictStatus::Pass);
        assert_eq!(v.summary, "all good");
        assert!(v.is_pass());
        assert!(v.findings.is_empty());
    }

    #[test]
    fn verdict_fail_sets_signature_and_findings() {
        let findings = vec![Finding {
            check_id: "paths".into(),
            kind: FindingKind::MissingPath,
            message: "missing".into(),
            detail: None,
        }];
        let v = Verdict::fail("not ok", findings.clone(), Some("log text".into()));
        assert_eq!(v.status, VerdictStatus::Fail);
        assert!(!v.is_pass());
        assert_eq!(v.findings.len(), 1);
        assert!(v.signature.is_some());
    }

    #[test]
    fn verdict_error_sets_signature_from_summary() {
        let v = Verdict::error("command died");
        assert_eq!(v.status, VerdictStatus::Error);
        assert!(!v.is_pass());
        assert_eq!(v.signature.as_deref(), Some("error:command died"));
    }

    #[test]
    fn signature_for_stable_across_calls() {
        let findings = vec![Finding {
            check_id: "paths".into(),
            kind: FindingKind::MissingPath,
            message: "missing".into(),
            detail: None,
        }];
        let sig1 = signature_for(&findings, Some("log"));
        let sig2 = signature_for(&findings, Some("log"));
        assert_eq!(sig1, sig2, "signature must be deterministic");
        assert!(!sig1.is_empty());
    }

    #[test]
    fn signature_for_produces_non_empty_hex() {
        let findings = vec![Finding {
            check_id: "x".into(),
            kind: FindingKind::MissingPath,
            message: "m".into(),
            detail: None,
        }];
        let sig = signature_for(&findings, None);
        assert!(!sig.is_empty());
        assert!(sig.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn default_id_paths_exist_is_correct() {
        assert_eq!(default_id_paths_exist(), "paths_exist");
    }

    #[test]
    fn default_id_paths_absent_is_correct() {
        assert_eq!(default_id_paths_absent(), "paths_absent");
    }

    #[test]
    fn default_id_content_contains_is_correct() {
        assert_eq!(default_id_content_contains(), "content_contains");
    }

    #[test]
    fn default_id_command_is_correct() {
        assert_eq!(default_id_command(), "command");
    }

    #[test]
    fn default_id_git_diff_is_correct() {
        assert_eq!(default_id_git_diff(), "has_diff");
    }

    #[test]
    fn default_true_is_true() {
        assert!(default_true());
    }

    #[test]
    fn pipeline_result_is_pass_when_overall_is_pass() {
        let r = PipelineResult {
            overall: VerdictStatus::Pass,
            results: vec![],
            combined_findings: vec![],
            combined_signature: None,
        };
        assert!(r.is_pass());
    }

    #[test]
    fn pipeline_result_is_not_pass_when_overall_is_fail() {
        let r = PipelineResult {
            overall: VerdictStatus::Fail,
            results: vec![],
            combined_findings: vec![],
            combined_signature: None,
        };
        assert!(!r.is_pass());
    }

    #[test]
    fn pipeline_result_repair_feedback_includes_failed_results() {
        let r = PipelineResult {
            overall: VerdictStatus::Fail,
            results: vec![
                NamedVerdict {
                    id: "check1".into(),
                    kind: "command".into(),
                    verdict: Verdict::pass("ok"),
                },
                NamedVerdict {
                    id: "check2".into(),
                    kind: "command".into(),
                    verdict: Verdict::fail("bad", vec![], None),
                },
            ],
            combined_findings: vec![],
            combined_signature: None,
        };
        let fb = r.repair_feedback();
        assert!(fb.contains("validation failed"));
        assert!(fb.contains("check2"));
        assert!(!fb.contains("check1"), "passing check should not appear in feedback");
    }

    #[test]
    fn pipeline_result_repair_feedback_uses_combined_when_present() {
        let r = PipelineResult {
            overall: VerdictStatus::Fail,
            results: vec![],
            combined_findings: vec![Finding {
                check_id: "combined".into(),
                kind: FindingKind::MissingPath,
                message: "gone".into(),
                detail: None,
            }],
            combined_signature: Some("abc".into()),
        };
        let fb = r.repair_feedback();
        assert!(fb.contains("gone"));
    }
}
