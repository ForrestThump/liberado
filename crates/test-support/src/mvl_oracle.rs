//! Path-based MVL conformance oracle.
//!
//! Input is on-disk JSONL only. This module does not import Liberado session types,
//! `CoderEvent`, or coding-pack internals. Any harness that writes `*.mvl.jsonl`
//! (and optionally a paired `*.execution.jsonl`) can be judged by the same entry.
//!
//! ## Foreign-harness invocation
//!
//! After a producer writes `$OUT/run.mvl.jsonl` (and optionally `$OUT/run.execution.jsonl`):
//!
//! ```text
//! cargo run -p liberado-test-support --bin mvl-conformance -- \
//!   --mvl $OUT/run.mvl.jsonl \
//!   --execution $OUT/run.execution.jsonl \
//!   --expected-content-shown <call_id>=<path-to-bytes>
//!
//! cargo test -p liberado-test-support --test mvl_e2e_oracle -- --nocapture
//! ```
//!
//! Honesty checks need `--expected-content-shown` (or [`ConformanceOpts::expected_content_shown`]).
//! Without ground-truth bytes the honesty rule is skipped, not passed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::trace_contracts::{
    JsonlEvent, assert_attempt_brackets, assert_crash_survival, assert_join_integrity,
    assert_mvl_has_no_scheduler_leakage, assert_seq_gap_free, assert_system_prompt_once,
    assert_tool_catalog_once, assert_tool_honesty, assert_tools_changed_covers_offered_diff,
    reconstruct_all_turns,
};

/// One of the eight Conformance rules in `docs/spec/reference/model-view-log.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConformanceRule {
    Reconstruction,
    CrashSurvival,
    Ordering,
    SystemPromptRecoverable,
    ToolCatalogueRecoverable,
    ToolHonesty,
    WithdrawalVisible,
    JoinIntegrity,
}

impl ConformanceRule {
    pub const ALL: [ConformanceRule; 8] = [
        ConformanceRule::Reconstruction,
        ConformanceRule::CrashSurvival,
        ConformanceRule::Ordering,
        ConformanceRule::SystemPromptRecoverable,
        ConformanceRule::ToolCatalogueRecoverable,
        ConformanceRule::ToolHonesty,
        ConformanceRule::WithdrawalVisible,
        ConformanceRule::JoinIntegrity,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ConformanceRule::Reconstruction => "reconstruction",
            ConformanceRule::CrashSurvival => "crash_survival",
            ConformanceRule::Ordering => "ordering",
            ConformanceRule::SystemPromptRecoverable => "system_prompt_recoverable",
            ConformanceRule::ToolCatalogueRecoverable => "tool_catalogue_recoverable",
            ConformanceRule::ToolHonesty => "tool_honesty",
            ConformanceRule::WithdrawalVisible => "withdrawal_visible",
            ConformanceRule::JoinIntegrity => "join_integrity",
        }
    }
}

/// Outcome of one rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerdictStatus {
    Pass,
    Fail,
    Skipped,
}

/// Pass, fail, or skip for one Conformance rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RuleVerdict {
    pub rule: ConformanceRule,
    pub status: VerdictStatus,
    pub detail: String,
}

/// Structured report covering all eight rules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConformanceReport {
    pub mvl_path: String,
    pub execution_path: Option<String>,
    pub verdicts: Vec<RuleVerdict>,
}

impl ConformanceReport {
    pub fn verdict(&self, rule: ConformanceRule) -> Option<&RuleVerdict> {
        self.verdicts.iter().find(|v| v.rule == rule)
    }

    pub fn failed(&self) -> Vec<&RuleVerdict> {
        self.verdicts
            .iter()
            .filter(|v| v.status == VerdictStatus::Fail)
            .collect()
    }

    pub fn all_checked_passed(&self) -> bool {
        self.verdicts
            .iter()
            .all(|v| v.status != VerdictStatus::Fail)
    }
}

/// Optional inputs for honesty, join, and simulated crash-prefix.
#[derive(Debug, Clone, Default)]
pub struct ConformanceOpts {
    pub execution_path: Option<PathBuf>,
    /// `call_id` → exact bytes the tool layer handed the model.
    pub expected_content_shown: BTreeMap<String, String>,
    /// If set, judge only events with `seq <= n` (durable prefix after a kill).
    pub kill_after_seq: Option<i64>,
}

fn verdict(rule: ConformanceRule, result: Result<String, String>) -> RuleVerdict {
    match result {
        Ok(detail) => RuleVerdict {
            rule,
            status: VerdictStatus::Pass,
            detail,
        },
        Err(detail) => RuleVerdict {
            rule,
            status: VerdictStatus::Fail,
            detail,
        },
    }
}

fn skipped(rule: ConformanceRule, detail: impl Into<String>) -> RuleVerdict {
    RuleVerdict {
        rule,
        status: VerdictStatus::Skipped,
        detail: detail.into(),
    }
}

fn apply_kill_prefix(events: Vec<JsonlEvent>, kill_after_seq: Option<i64>) -> Vec<JsonlEvent> {
    match kill_after_seq {
        Some(n) => events.into_iter().filter(|e| e.seq <= n).collect(),
        None => events,
    }
}

/// Judge an on-disk MVL file (and optional execution file) against all eight rules.
///
/// I/O failures return `Err`. Rule failures stay in the report as `Fail` verdicts.
pub fn run_mvl_conformance(
    mvl_path: &Path,
    opts: &ConformanceOpts,
) -> Result<ConformanceReport, String> {
    let raw = std::fs::read_to_string(mvl_path)
        .map_err(|e| format!("read {}: {e}", mvl_path.display()))?;

    let mut verdicts = Vec::with_capacity(8);

    let parsed = assert_crash_survival(&raw);
    let events = match parsed {
        Ok(events) => {
            let events = apply_kill_prefix(events, opts.kill_after_seq);
            let detail = match opts.kill_after_seq {
                Some(n) => format!(
                    "complete JSONL lines parse; kill_after_seq={n}; retained {}",
                    events.len()
                ),
                None => format!("complete JSONL lines parse; {} events", events.len()),
            };
            verdicts.push(verdict(ConformanceRule::CrashSurvival, Ok(detail)));
            events
        }
        Err(e) => {
            verdicts.push(verdict(ConformanceRule::CrashSurvival, Err(e)));
            for rule in ConformanceRule::ALL {
                if rule != ConformanceRule::CrashSurvival {
                    verdicts.push(skipped(rule, "mvl did not parse as complete JSONL"));
                }
            }
            return Ok(ConformanceReport {
                mvl_path: mvl_path.display().to_string(),
                execution_path: opts
                    .execution_path
                    .as_ref()
                    .map(|p| p.display().to_string()),
                verdicts,
            });
        }
    };

    verdicts.push(verdict(
        ConformanceRule::Ordering,
        assert_seq_gap_free(&events).map(|()| "seq is gap-free and monotonic".into()),
    ));

    verdicts.push(verdict(
        ConformanceRule::Reconstruction,
        reconstruct_all_turns(&events).map(|turns| {
            format!(
                "reconstructed {} turn(s); system, catalog, messages, params, tools_offered recovered",
                turns.len()
            )
        }),
    ));

    verdicts.push(verdict(
        ConformanceRule::SystemPromptRecoverable,
        assert_system_prompt_once(&events)
            .map(|()| "each system hash appears in full exactly once".into()),
    ));

    verdicts.push(verdict(
        ConformanceRule::ToolCatalogueRecoverable,
        assert_tool_catalog_once(&events)
            .map(|()| "each catalog hash appears in full exactly once".into()),
    ));

    if opts.expected_content_shown.is_empty() {
        verdicts.push(skipped(
            ConformanceRule::ToolHonesty,
            "no --expected-content-shown / expected_content_shown supplied",
        ));
    } else {
        verdicts.push(verdict(
            ConformanceRule::ToolHonesty,
            assert_tool_honesty(&events, &opts.expected_content_shown).map(|()| {
                format!(
                    "content_shown matches ground truth for {} call_id(s)",
                    opts.expected_content_shown.len()
                )
            }),
        ));
    }

    verdicts.push(verdict(
        ConformanceRule::WithdrawalVisible,
        assert_tools_changed_covers_offered_diff(&events)
            .map(|()| "offered-set diffs are covered by tools_changed".into()),
    ));

    match &opts.execution_path {
        None => verdicts.push(skipped(
            ConformanceRule::JoinIntegrity,
            "no --execution / execution_path supplied",
        )),
        Some(exec_path) => {
            let exec_raw = std::fs::read_to_string(exec_path)
                .map_err(|e| format!("read {}: {e}", exec_path.display()))?;
            match assert_crash_survival(&exec_raw) {
                Err(e) => verdicts.push(verdict(
                    ConformanceRule::JoinIntegrity,
                    Err(format!("execution log failed crash parse: {e}")),
                )),
                Ok(exec_events) => {
                    let exec_events = apply_kill_prefix(exec_events, opts.kill_after_seq);
                    let join = assert_seq_gap_free(&exec_events)
                        .and_then(|()| assert_attempt_brackets(&exec_events))
                        .and_then(|()| assert_join_integrity(&events, &exec_events))
                        .and_then(|()| assert_mvl_has_no_scheduler_leakage(&events))
                        .map(|()| {
                            "execution events join MVL by call_id / turn; no timestamp inference"
                                .into()
                        });
                    verdicts.push(verdict(ConformanceRule::JoinIntegrity, join));
                }
            }
        }
    }

    Ok(ConformanceReport {
        mvl_path: mvl_path.display().to_string(),
        execution_path: opts
            .execution_path
            .as_ref()
            .map(|p| p.display().to_string()),
        verdicts,
    })
}

/// Parse CLI arguments for the `mvl-conformance` binary.
pub fn parse_oracle_args<I, S>(args: I) -> Result<(PathBuf, ConformanceOpts), String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut mvl: Option<PathBuf> = None;
    let mut opts = ConformanceOpts::default();
    let mut iter = args.into_iter().peekable();
    while let Some(raw) = iter.next() {
        let arg = raw.as_ref();
        match arg {
            "--mvl" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "--mvl requires a path".to_string())?;
                mvl = Some(PathBuf::from(value.as_ref()));
            }
            "--execution" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "--execution requires a path".to_string())?;
                opts.execution_path = Some(PathBuf::from(value.as_ref()));
            }
            "--expected-content-shown" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "--expected-content-shown requires call_id=path".to_string())?;
                let value = value.as_ref();
                let (id, path) = value.split_once('=').ok_or_else(|| {
                    format!("--expected-content-shown expected call_id=path, got {value}")
                })?;
                let bytes = std::fs::read_to_string(path)
                    .map_err(|e| format!("read honesty file {path}: {e}"))?;
                opts.expected_content_shown.insert(id.to_string(), bytes);
            }
            "--kill-after-seq" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "--kill-after-seq requires an integer".to_string())?;
                let n: i64 = value
                    .as_ref()
                    .parse()
                    .map_err(|e| format!("--kill-after-seq: parse {}: {e}", value.as_ref()))?;
                opts.kill_after_seq = Some(n);
            }
            "--help" | "-h" => {
                return Err(oracle_usage().into());
            }
            other => return Err(format!("unknown argument: {other}\n{}", oracle_usage())),
        }
    }
    let mvl = mvl.ok_or_else(|| format!("--mvl is required\n{}", oracle_usage()))?;
    Ok((mvl, opts))
}

pub fn oracle_usage() -> &'static str {
    "mvl-conformance --mvl <path> [--execution <path>] \
     [--expected-content-shown <call_id>=<path>]... [--kill-after-seq <n>]"
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_temp(name: &str, body: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("liberado-mvl-oracle-unit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        f.flush().unwrap();
        path
    }

    #[test]
    fn parse_args_requires_mvl() {
        let err = parse_oracle_args(["--execution", "x.jsonl"]).unwrap_err();
        assert!(err.contains("--mvl"), "{err}");
    }

    #[test]
    fn parse_args_reads_honesty_file() {
        let truth = write_temp("truth.txt", "hit");
        let (mvl, opts) = parse_oracle_args([
            "--mvl",
            "run.mvl.jsonl",
            "--expected-content-shown",
            &format!("c1={}", truth.display()),
            "--kill-after-seq",
            "3",
        ])
        .unwrap();
        assert_eq!(mvl, PathBuf::from("run.mvl.jsonl"));
        assert_eq!(opts.expected_content_shown.get("c1").unwrap(), "hit");
        assert_eq!(opts.kill_after_seq, Some(3));
    }

    /// Every rule's wire string — the spelling foreign harnesses see in reports and pin their
    /// tooling to.
    #[test]
    fn as_str_is_the_stable_wire_name_for_every_rule() {
        use ConformanceRule::*;
        assert_eq!(Reconstruction.as_str(), "reconstruction");
        assert_eq!(CrashSurvival.as_str(), "crash_survival");
        assert_eq!(Ordering.as_str(), "ordering");
        assert_eq!(
            SystemPromptRecoverable.as_str(),
            "system_prompt_recoverable"
        );
        assert_eq!(
            ToolCatalogueRecoverable.as_str(),
            "tool_catalogue_recoverable"
        );
        assert_eq!(ToolHonesty.as_str(), "tool_honesty");
        assert_eq!(WithdrawalVisible.as_str(), "withdrawal_visible");
        assert_eq!(JoinIntegrity.as_str(), "join_integrity");
        // ALL and as_str must agree: a rule added without a wire name would report as "".
        for rule in ConformanceRule::ALL {
            assert!(!rule.as_str().is_empty());
        }
    }
}
