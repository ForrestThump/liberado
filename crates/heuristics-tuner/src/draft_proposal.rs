//! Meta-loop export: turn tuner eval deltas into **draft proposal artifacts** (Decision 14).
//!
//! Proposes only — never writes `prompts/**` into the live Liberado tree, never opens PRs, never
//! widens authority. A human (or PR-dispatch after human freeze) must adopt the change.
//!
//! Artifacts written under the tuner run directory:
//! - `PROPOSAL.md` — human summary
//! - `proposal.json` — machine-readable metadata
//! - `proposed/<target_path>` — proposed file body (e.g. `prompts/coder/coder.md`)
//! - `pr_factory_task.json` — payload shape for `submit_pr_factory_task` (optional hand-off)

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::candidate::Candidate;
use crate::coder_scoring::CoderFitness;

/// Default path the coder-layer proposal targets (repo-relative).
pub const DEFAULT_CODER_PROMPT_PATH: &str = "prompts/coder/coder.md";

/// Structured draft from a completed coder tuning session.
#[derive(Debug, Clone, Serialize)]
pub struct CoderDraftProposal {
    /// Layer that produced this proposal (`coder`).
    pub layer: String,
    /// Whether the winner strictly improves baseline accuracy without unsafe acts.
    pub recommended: bool,
    pub reason: String,
    /// Repo-relative file the human should replace if they accept.
    pub target_path: String,
    pub baseline_accuracy: f32,
    pub winner_accuracy: f32,
    pub baseline_nonempty_diff_rate: f32,
    pub winner_nonempty_diff_rate: f32,
    pub baseline_unsafe_acts: usize,
    pub winner_unsafe_acts: usize,
    pub improved_scenarios: Vec<String>,
    pub regressed_scenarios: Vec<String>,
    /// Full proposed system prompt body (file content).
    pub proposed_prompt: String,
    /// Baseline prompt (for human diff).
    pub baseline_prompt: String,
    /// Decision 14 notice embedded for every consumer.
    pub policy: String,
}

/// PR-factory submit payload (JSON-compatible with liberado-pr-dispatch `submit_pr_factory_task`).
#[derive(Debug, Clone, Serialize)]
pub struct PrFactoryTaskPayload {
    pub description: String,
    pub context: String,
    pub risk_level: String,
    pub success_criteria: Vec<String>,
    pub verifiers: Vec<serde_json::Value>,
    pub verify_profile: Option<String>,
    /// Echo of proposal for operators; not a factory field.
    pub _meta: PrFactoryTaskMeta,
}

#[derive(Debug, Clone, Serialize)]
pub struct PrFactoryTaskMeta {
    pub source: String,
    pub recommended: bool,
    pub target_path: String,
    pub decision: String,
}

/// Build a draft proposal from coder tuner fitness deltas. Pure — no IO.
pub fn build_coder_draft_proposal(
    winner: &Candidate,
    winner_fitness: &CoderFitness,
    baseline_fitness: &CoderFitness,
    baseline_prompt: &str,
    target_path: &str,
) -> CoderDraftProposal {
    let improved = scenario_improvements(baseline_fitness, winner_fitness);
    let regressed = scenario_regressions(baseline_fitness, winner_fitness);

    let accuracy_up = winner_fitness.accuracy > baseline_fitness.accuracy + 0.001;
    let no_unsafe = winner_fitness.unsafe_acts == 0;
    let no_regression = regressed.is_empty();
    let prompt_changed = winner.prompt.trim() != baseline_prompt.trim();

    let recommended = accuracy_up && no_unsafe && no_regression && prompt_changed;

    let reason = if !prompt_changed {
        "winner prompt identical to baseline — no proposal needed".into()
    } else if winner_fitness.unsafe_acts > 0 {
        "winner has unsafe path touches — never recommend auto-adoption".into()
    } else if !no_regression {
        format!(
            "accuracy may change but scenarios regressed: {}",
            regressed.join(", ")
        )
    } else if !accuracy_up {
        format!(
            "no accuracy improvement ({:.2} -> {:.2}); keep as optional experiment only",
            baseline_fitness.accuracy, winner_fitness.accuracy
        )
    } else {
        format!(
            "accuracy {:.2} -> {:.2} with no unsafe acts and no scenario regressions",
            baseline_fitness.accuracy, winner_fitness.accuracy
        )
    };

    CoderDraftProposal {
        layer: "coder".into(),
        recommended,
        reason,
        target_path: target_path.into(),
        baseline_accuracy: baseline_fitness.accuracy,
        winner_accuracy: winner_fitness.accuracy,
        baseline_nonempty_diff_rate: baseline_fitness.nonempty_diff_rate,
        winner_nonempty_diff_rate: winner_fitness.nonempty_diff_rate,
        baseline_unsafe_acts: baseline_fitness.unsafe_acts,
        winner_unsafe_acts: winner_fitness.unsafe_acts,
        improved_scenarios: improved,
        regressed_scenarios: regressed,
        proposed_prompt: winner.prompt.clone(),
        baseline_prompt: baseline_prompt.to_string(),
        policy: "Decision 14: heuristics-tuner proposes only. Humans dispose. Never auto-merge prompts or widen authority.".into(),
    }
}

fn scenario_improvements(baseline: &CoderFitness, winner: &CoderFitness) -> Vec<String> {
    let mut out = Vec::new();
    for w in &winner.scenarios {
        if let Some(b) = baseline.scenarios.iter().find(|b| b.name == w.name) {
            if w.pass_rate() > b.pass_rate() + 0.01 {
                out.push(format!(
                    "{} ({:.2} -> {:.2})",
                    w.name,
                    b.pass_rate(),
                    w.pass_rate()
                ));
            }
        }
    }
    out
}

fn scenario_regressions(baseline: &CoderFitness, winner: &CoderFitness) -> Vec<String> {
    let mut out = Vec::new();
    for w in &winner.scenarios {
        if let Some(b) = baseline.scenarios.iter().find(|b| b.name == w.name) {
            if w.pass_rate() + 0.01 < b.pass_rate() {
                out.push(format!(
                    "{} ({:.2} -> {:.2})",
                    w.name,
                    b.pass_rate(),
                    w.pass_rate()
                ));
            }
        }
    }
    out
}

/// Human-readable PROPOSAL.md body.
pub fn format_proposal_markdown(p: &CoderDraftProposal) -> String {
    let mut out = String::new();
    out.push_str("# Heuristics tuner draft proposal (coder layer)\n\n");
    out.push_str(&format!("**Recommended for adoption:** {}\n\n", p.recommended));
    out.push_str(&format!("**Reason:** {}\n\n", p.reason));
    out.push_str(&format!("**Policy:** {}\n\n", p.policy));
    out.push_str("## Metrics\n\n");
    out.push_str(&format!(
        "| Metric | Baseline | Winner |\n|---|---|---|\n| accuracy | {:.2} | {:.2} |\n| nonempty-diff | {:.2} | {:.2} |\n| unsafe acts | {} | {} |\n\n",
        p.baseline_accuracy,
        p.winner_accuracy,
        p.baseline_nonempty_diff_rate,
        p.winner_nonempty_diff_rate,
        p.baseline_unsafe_acts,
        p.winner_unsafe_acts
    ));
    out.push_str("## Scenario deltas\n\n");
    if p.improved_scenarios.is_empty() {
        out.push_str("- Improved: none\n");
    } else {
        out.push_str("- Improved:\n");
        for s in &p.improved_scenarios {
            out.push_str(&format!("  - {s}\n"));
        }
    }
    if p.regressed_scenarios.is_empty() {
        out.push_str("- Regressed: none\n");
    } else {
        out.push_str("- Regressed:\n");
        for s in &p.regressed_scenarios {
            out.push_str(&format!("  - {s}\n"));
        }
    }
    out.push_str(&format!(
        "\n## Target file\n\n`{}`\n\n",
        p.target_path
    ));
    out.push_str("## How to adopt\n\n");
    out.push_str("1. Review `proposed/` content vs baseline.\n");
    out.push_str("2. If recommended, copy into the repo path by hand **or** submit `pr_factory_task.json` via PR-dispatch as a human-approved draft PR task.\n");
    out.push_str("3. Do **not** auto-merge; re-run mock curriculum after adoption:\n");
    out.push_str("   `cargo test -p liberado-heuristics-tuner --lib mock_curriculum`\n\n");
    out.push_str("## Proposed prompt\n\n```\n");
    out.push_str(&p.proposed_prompt);
    out.push_str("\n```\n");
    out
}

/// Build a PR-factory task description that asks Liberado loop to apply the proposed prompt file.
pub fn build_pr_factory_task(p: &CoderDraftProposal) -> PrFactoryTaskPayload {
    let description = format!(
        "Apply heuristics-tuner draft proposal for the coder system prompt.\n\
         Replace the contents of `{}` with the proposed prompt from this task context.\n\
         Do not modify any other files. Do not change config/policy. Decision 14: this is a draft PR only.",
        p.target_path
    );
    let context = format!(
        "Source: liberado-heuristics-tuner meta-loop export.\n\
         Recommended: {}.\n\
         Reason: {}.\n\
         Metrics: accuracy {:.2} -> {:.2}, unsafe {} -> {}.\n\n\
         === PROPOSED FILE BODY for {} ===\n{}\n=== END PROPOSED FILE ===\n",
        p.recommended,
        p.reason,
        p.baseline_accuracy,
        p.winner_accuracy,
        p.baseline_unsafe_acts,
        p.winner_unsafe_acts,
        p.target_path,
        p.proposed_prompt
    );

    PrFactoryTaskPayload {
        description,
        context,
        risk_level: if p.recommended {
            "medium".into()
        } else {
            "high".into()
        },
        success_criteria: vec![
            format!("{} updated to the proposed coder system prompt", p.target_path),
            "no unrelated files modified".into(),
        ],
        verifiers: vec![
            serde_json::json!({
                "type": "paths_exist",
                "id": "prompt_path",
                "paths": [p.target_path]
            }),
            serde_json::json!({
                "type": "content_contains",
                "id": "prompt_marker",
                // First non-empty line of proposed prompt as a weak content check.
                "path": p.target_path,
                "must_include": [first_meaningful_line(&p.proposed_prompt)]
            }),
            serde_json::json!({
                "type": "git_nonempty_diff",
                "id": "has_diff"
            }),
        ],
        verify_profile: None,
        _meta: PrFactoryTaskMeta {
            source: "liberado-heuristics-tuner".into(),
            recommended: p.recommended,
            target_path: p.target_path.clone(),
            decision: "Decision 14 — propose only; human dispose".into(),
        },
    }
}

fn first_meaningful_line(prompt: &str) -> String {
    prompt
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("Liberado")
        .chars()
        .take(80)
        .collect()
}

/// Write all draft artifacts under `out_dir`. Returns paths written.
pub async fn write_coder_draft_proposal(
    out_dir: &Path,
    proposal: &CoderDraftProposal,
) -> std::io::Result<Vec<PathBuf>> {
    let mut written = Vec::new();

    let md_path = out_dir.join("PROPOSAL.md");
    tokio::fs::write(&md_path, format_proposal_markdown(proposal)).await?;
    written.push(md_path);

    let json_path = out_dir.join("proposal.json");
    let json = serde_json::to_string_pretty(proposal)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    tokio::fs::write(&json_path, json).await?;
    written.push(json_path);

    let proposed_file = out_dir.join("proposed").join(&proposal.target_path);
    if let Some(parent) = proposed_file.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(&proposed_file, &proposal.proposed_prompt).await?;
    written.push(proposed_file);

    let task = build_pr_factory_task(proposal);
    let task_path = out_dir.join("pr_factory_task.json");
    let task_json = serde_json::to_string_pretty(&task)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    tokio::fs::write(&task_path, task_json).await?;
    written.push(task_path);

    // Unified-diff style text for quick review (not a git apply patch).
    let diff_path = out_dir.join("proposed_prompt.diff.txt");
    tokio::fs::write(
        &diff_path,
        format!(
            "--- a/{}\n+++ b/{}\n@@ baseline -> winner @@\n\
             - (baseline length {} chars)\n+ (winner length {} chars)\n\n\
             # Full winner body is in proposed/{}\n# Full baseline is in proposal.json baseline_prompt\n",
            proposal.target_path,
            proposal.target_path,
            proposal.baseline_prompt.len(),
            proposal.proposed_prompt.len(),
            proposal.target_path
        ),
    )
    .await?;
    written.push(diff_path);

    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::candidate::CandidateOrigin;
    use crate::coder_scenarios::CoderExpect;
    use crate::coder_scoring::{CoderScoredScenario, CoderTrial, CoderTrialOutcome};
    use liberado_common::Outcome;

    fn fitness(acc: f32, unsafe_acts: usize, scenario_pass: f32) -> CoderFitness {
        CoderFitness {
            accuracy: acc,
            outcome_match_rate: acc,
            nonempty_diff_rate: acc,
            unsafe_acts,
            scenarios: vec![CoderScoredScenario {
                name: "create-hello-file",
                task: "t",
                note: "n",
                expect: CoderExpect {
                    must_change: &[],
                    must_not_change: &[],
                    content_contains: &[],
                    require_nonempty_diff: true,
                    expected_outcome: Outcome::Succeeded,
                },
                trials: vec![CoderTrial {
                    model: "m".into(),
                    outcome: CoderTrialOutcome {
                        passed: scenario_pass >= 0.99,
                        unsafe_touch: false,
                        outcome_matched: true,
                        nonempty_diff: true,
                    },
                }],
            }],
        }
    }

    #[test]
    fn recommends_when_accuracy_up_and_safe() {
        let baseline = fitness(0.5, 0, 0.0);
        let winner_fit = fitness(1.0, 0, 1.0);
        let winner = Candidate {
            prompt: "better prompt for Liberado coding worker".into(),
            origin: CandidateOrigin::ColdStart,
        };
        let p = build_coder_draft_proposal(
            &winner,
            &winner_fit,
            &baseline,
            "old prompt",
            DEFAULT_CODER_PROMPT_PATH,
        );
        assert!(p.recommended);
        assert!(p.improved_scenarios.iter().any(|s| s.contains("create-hello-file")));
        assert!(p.regressed_scenarios.is_empty());
    }

    #[test]
    fn does_not_recommend_identical_prompt() {
        let fit = fitness(1.0, 0, 1.0);
        let winner = Candidate {
            prompt: "same".into(),
            origin: CandidateOrigin::ColdStart,
        };
        let p = build_coder_draft_proposal(&winner, &fit, &fit, "same", DEFAULT_CODER_PROMPT_PATH);
        assert!(!p.recommended);
    }

    #[test]
    fn does_not_recommend_unsafe_winner() {
        let baseline = fitness(0.5, 0, 0.0);
        let winner_fit = fitness(1.0, 1, 1.0);
        let winner = Candidate {
            prompt: "new".into(),
            origin: CandidateOrigin::ColdStart,
        };
        let p = build_coder_draft_proposal(
            &winner,
            &winner_fit,
            &baseline,
            "old",
            DEFAULT_CODER_PROMPT_PATH,
        );
        assert!(!p.recommended);
        assert!(p.reason.contains("unsafe"));
    }

    #[test]
    fn pr_factory_task_includes_verifiers_and_policy() {
        let baseline = fitness(0.5, 0, 0.0);
        let winner_fit = fitness(1.0, 0, 1.0);
        let winner = Candidate {
            prompt: "You are Liberado coding worker improved.\n".into(),
            origin: CandidateOrigin::ColdStart,
        };
        let p = build_coder_draft_proposal(
            &winner,
            &winner_fit,
            &baseline,
            "old",
            DEFAULT_CODER_PROMPT_PATH,
        );
        let task = build_pr_factory_task(&p);
        assert!(task.description.contains("prompts/coder/coder.md"));
        assert_eq!(task.risk_level, "medium");
        assert!(!task.verifiers.is_empty());
        assert!(task._meta.decision.contains("Decision 14"));
    }

    #[tokio::test]
    async fn write_creates_expected_files() {
        let dir = tempfile::tempdir().unwrap();
        let baseline = fitness(0.5, 0, 0.0);
        let winner_fit = fitness(1.0, 0, 1.0);
        let winner = Candidate {
            prompt: "proposed body\n".into(),
            origin: CandidateOrigin::ColdStart,
        };
        let p = build_coder_draft_proposal(
            &winner,
            &winner_fit,
            &baseline,
            "baseline body\n",
            DEFAULT_CODER_PROMPT_PATH,
        );
        let written = write_coder_draft_proposal(dir.path(), &p).await.unwrap();
        assert!(written.len() >= 4);
        assert!(dir.path().join("PROPOSAL.md").is_file());
        assert!(dir.path().join("proposal.json").is_file());
        assert!(dir
            .path()
            .join("proposed/prompts/coder/coder.md")
            .is_file());
        assert!(dir.path().join("pr_factory_task.json").is_file());
        let body = std::fs::read_to_string(dir.path().join("proposed/prompts/coder/coder.md")).unwrap();
        assert_eq!(body, "proposed body\n");
    }
}
