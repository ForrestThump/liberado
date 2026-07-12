//! Format the tuning session's result into the artifact a human reads before deciding whether to
//! hand-copy the winning prompt into `liberado_dispatcher::DEFAULT_SYSTEM_PROMPT`. Nothing in this
//! crate ever writes to the real dispatcher — this is the "propose a diff + rubric, never
//! auto-merge" half of `docs/roadmap/heuristics-tuning-engine-plan.md`'s design.

use std::fmt::Write as _;

use liberado_dispatcher::DEFAULT_SYSTEM_PROMPT;

use crate::candidate::{Candidate, CandidateOrigin};
use crate::coder_scoring::CoderFitness;
use crate::scoring::CandidateFitness;
use crate::tool_loop_scoring::ToolLoopFitness;

/// Build the full rubric text. Pure function of its inputs, so it's directly unit-testable with
/// canned fitness values — no need to run a real tuning session to check the formatting.
pub fn format_rubric(
    winner: &Candidate,
    winner_fitness: &CandidateFitness,
    baseline_fitness: &CandidateFitness,
    justification: Option<&str>,
) -> String {
    let mut out = String::new();

    let _ = writeln!(out, "=== Heuristics Tuner Proposal ===\n");

    match &winner.origin {
        CandidateOrigin::ColdStart => {
            let _ = writeln!(out, "Winner origin: cold start (independent of the current best)");
        }
        CandidateOrigin::MutatedFrom {
            parent_index,
            parent_accuracy,
        } => {
            let _ = writeln!(
                out,
                "Winner origin: mutated from beam candidate #{parent_index} (accuracy {parent_accuracy:.2})"
            );
        }
    }

    let _ = writeln!(out, "\n-- Metric deltas (baseline -> candidate) --");
    let _ = writeln!(
        out,
        "routing accuracy  : {:.2} -> {:.2}",
        baseline_fitness.accuracy, winner_fitness.accuracy
    );
    let _ = writeln!(
        out,
        "safe-default rate : {:.2} -> {:.2}",
        baseline_fitness.safe_default_rate, winner_fitness.safe_default_rate
    );
    let _ = writeln!(
        out,
        "unsafe acts       : {} -> {}   (must stay 0 — a nonzero winner is never selected)",
        baseline_fitness.unsafe_acts, winner_fitness.unsafe_acts
    );

    // A scenario "fails" for diffing purposes at the same majority threshold `CandidateFitness::failing`
    // uses — one consistent cutoff for every consumer that needs a scenario to collapse to pass/fail.
    let baseline_failing: std::collections::HashSet<&str> = baseline_fitness
        .scenarios
        .iter()
        .filter(|s| s.pass_rate() <= 0.5)
        .map(|s| s.name)
        .collect();
    let winner_failing: std::collections::HashSet<&str> = winner_fitness
        .scenarios
        .iter()
        .filter(|s| s.pass_rate() <= 0.5)
        .map(|s| s.name)
        .collect();

    let now_passing: Vec<&str> = baseline_failing.difference(&winner_failing).copied().collect();
    let now_failing: Vec<&str> = winner_failing.difference(&baseline_failing).copied().collect();

    let _ = writeln!(out, "\n-- Scenario changes --");
    if now_passing.is_empty() {
        let _ = writeln!(out, "Now passing (were failing): none");
    } else {
        let _ = writeln!(out, "Now passing (were failing):");
        for name in &now_passing {
            let _ = writeln!(out, "  - {name}");
        }
    }
    if now_failing.is_empty() {
        let _ = writeln!(out, "Now failing (were passing): none");
    } else {
        let _ = writeln!(out, "Now failing (were passing) [REGRESSION]:");
        for name in &now_failing {
            let _ = writeln!(out, "  - {name}");
        }
    }

    // Only scenarios with a mixed/partial result (not perfectly consistent) are worth surfacing
    // here — a scenario every trial agreed on either way is already covered by the pass/fail diff
    // above and would just be noise in this section.
    let mixed: Vec<&crate::scoring::ScoredScenario> = winner_fitness
        .scenarios
        .iter()
        .filter(|s| {
            let rate = s.pass_rate();
            rate > 0.0 && rate < 1.0
        })
        .collect();
    let _ = writeln!(out, "\n-- Per-model consistency (mixed results only) --");
    if mixed.is_empty() {
        let _ = writeln!(out, "none — every scenario agreed across all models/samples");
    } else {
        for s in &mixed {
            let _ = writeln!(out, "{}: {}", s.name, s.trial_breakdown());
        }
    }

    if let Some(text) = justification {
        let _ = writeln!(out, "\n-- Justification (from the tuning model) --\n{text}");
    } else {
        let _ = writeln!(
            out,
            "\n-- Justification --\n(unavailable — call budget was exhausted before this could be requested)"
        );
    }

    let _ = writeln!(out, "\n-- Candidate prompt --\n--- BEGIN ---\n{}\n--- END ---", winner.prompt);
    let _ = writeln!(
        out,
        "\n-- Baseline prompt (DEFAULT_SYSTEM_PROMPT) --\n--- BEGIN ---\n{DEFAULT_SYSTEM_PROMPT}\n--- END ---"
    );

    out
}

/// The executor/subagent-layer analog of [`format_rubric`] — same shape and same "propose, never
/// auto-merge" posture, but for a [`ToolLoopFitness`] instead of a dispatcher [`CandidateFitness`].
/// Takes `baseline_prompt` explicitly (rather than importing a specific const) so this one function
/// serves both `DIRECT_INSTRUCTIONS` (executor) and `SUBAGENT_PREAMBLE` (subagent) tuning sessions.
/// Coding-layer rubric (workspace diffs / path hygiene), parallel to [`format_executor_rubric`].
pub fn format_coder_rubric(
    winner: &Candidate,
    winner_fitness: &CoderFitness,
    baseline_fitness: &CoderFitness,
    baseline_prompt: &str,
    justification: Option<&str>,
) -> String {
    let mut out = String::new();

    let _ = writeln!(out, "=== Heuristics Tuner Proposal (coder layer) ===\n");
    let _ = writeln!(
        out,
        "Layer: Liberado coding worker system prompt (PR-dispatch / liberado-coder-agent).\n\
         Never auto-applied — hand-copy a winner into prompts/coder/coder.md or LIBERADO_CODER_PROMPT.\n"
    );

    match &winner.origin {
        CandidateOrigin::ColdStart => {
            let _ = writeln!(out, "Winner origin: cold start (independent of the current best)");
        }
        CandidateOrigin::MutatedFrom {
            parent_index,
            parent_accuracy,
        } => {
            let _ = writeln!(
                out,
                "Winner origin: mutated from beam candidate #{parent_index} (accuracy {parent_accuracy:.2})"
            );
        }
    }

    let _ = writeln!(out, "\n-- Metric deltas (baseline -> candidate) --");
    let _ = writeln!(
        out,
        "coding accuracy     : {:.2} -> {:.2}",
        baseline_fitness.accuracy, winner_fitness.accuracy
    );
    let _ = writeln!(
        out,
        "outcome-match rate  : {:.2} -> {:.2}",
        baseline_fitness.outcome_match_rate, winner_fitness.outcome_match_rate
    );
    let _ = writeln!(
        out,
        "nonempty-diff rate  : {:.2} -> {:.2}",
        baseline_fitness.nonempty_diff_rate, winner_fitness.nonempty_diff_rate
    );
    let _ = writeln!(
        out,
        "unsafe path touches : {} -> {}   (must stay 0 — a nonzero winner is never selected)",
        baseline_fitness.unsafe_acts, winner_fitness.unsafe_acts
    );

    let _ = writeln!(out, "\n-- Full scenario breakdown --");
    for s in &winner_fitness.scenarios {
        let _ = writeln!(
            out,
            "- {}: pass_rate={:.2} — {}",
            s.name,
            s.pass_rate(),
            s.diagnostic_breakdown()
        );
        let _ = writeln!(out, "  note: {}", s.note);
    }

    // Regressions / fixes vs baseline by name.
    let _ = writeln!(out, "\n-- Scenario changes vs baseline --");
    for w in &winner_fitness.scenarios {
        if let Some(b) = baseline_fitness
            .scenarios
            .iter()
            .find(|b| b.name == w.name)
        {
            let wp = w.pass_rate();
            let bp = b.pass_rate();
            if (wp - bp).abs() < 0.01 {
                continue;
            }
            let label = if wp > bp { "IMPROVED" } else { "REGRESSION" };
            let _ = writeln!(out, "- {} {label}: {:.2} -> {:.2}", w.name, bp, wp);
        }
    }

    let _ = writeln!(out, "\n-- Baseline prompt --\n```\n{baseline_prompt}\n```");
    let _ = writeln!(out, "\n-- Proposed coder system prompt --\n```\n{}\n```", winner.prompt);

    match justification {
        Some(j) if !j.trim().is_empty() => {
            let _ = writeln!(out, "\n-- Meta-model justification --\n{j}");
        }
        _ => {
            let _ = writeln!(out, "\n-- Meta-model justification --\nunavailable");
        }
    }

    out
}

pub fn format_executor_rubric(
    winner: &Candidate,
    winner_fitness: &ToolLoopFitness,
    baseline_fitness: &ToolLoopFitness,
    baseline_prompt: &str,
    justification: Option<&str>,
) -> String {
    let mut out = String::new();

    let _ = writeln!(out, "=== Heuristics Tuner Proposal (tool-loop layer) ===\n");

    match &winner.origin {
        CandidateOrigin::ColdStart => {
            let _ = writeln!(out, "Winner origin: cold start (independent of the current best)");
        }
        CandidateOrigin::MutatedFrom {
            parent_index,
            parent_accuracy,
        } => {
            let _ = writeln!(
                out,
                "Winner origin: mutated from beam candidate #{parent_index} (accuracy {parent_accuracy:.2})"
            );
        }
    }

    let _ = writeln!(out, "\n-- Metric deltas (baseline -> candidate) --");
    let _ = writeln!(
        out,
        "tool-loop accuracy : {:.2} -> {:.2}",
        baseline_fitness.accuracy, winner_fitness.accuracy
    );
    let _ = writeln!(
        out,
        "outcome-match rate : {:.2} -> {:.2}",
        baseline_fitness.outcome_match_rate, winner_fitness.outcome_match_rate
    );
    let _ = writeln!(
        out,
        "unsafe acts        : {} -> {}   (must stay 0 — a nonzero winner is never selected)",
        baseline_fitness.unsafe_acts, winner_fitness.unsafe_acts
    );

    let baseline_failing: std::collections::HashSet<&str> = baseline_fitness
        .scenarios
        .iter()
        .filter(|s| s.pass_rate() <= 0.5)
        .map(|s| s.name)
        .collect();
    let winner_failing: std::collections::HashSet<&str> = winner_fitness
        .scenarios
        .iter()
        .filter(|s| s.pass_rate() <= 0.5)
        .map(|s| s.name)
        .collect();

    let now_passing: Vec<&str> = baseline_failing.difference(&winner_failing).copied().collect();
    let now_failing: Vec<&str> = winner_failing.difference(&baseline_failing).copied().collect();

    let _ = writeln!(out, "\n-- Scenario changes --");
    if now_passing.is_empty() {
        let _ = writeln!(out, "Now passing (were failing): none");
    } else {
        let _ = writeln!(out, "Now passing (were failing):");
        for name in &now_passing {
            let _ = writeln!(out, "  - {name}");
        }
    }
    if now_failing.is_empty() {
        let _ = writeln!(out, "Now failing (were passing): none");
    } else {
        let _ = writeln!(out, "Now failing (were passing) [REGRESSION]:");
        for name in &now_failing {
            let _ = writeln!(out, "  - {name}");
        }
    }

    // Unconditional, every scenario — unlike the diffs above (which only show what *changed*) or
    // the mixed-results section below (only meaningful with >1 sample), this always tells a human
    // exactly which scenario is behind a given accuracy number and why, even on a single-sample run.
    let _ = writeln!(out, "\n-- Full scenario breakdown (winner) --");
    for s in &winner_fitness.scenarios {
        let _ = writeln!(out, "{}: pass_rate={:.2} — {}", s.name, s.pass_rate(), s.diagnostic_breakdown());
    }

    let mixed: Vec<&crate::tool_loop_scoring::ToolLoopScoredScenario> = winner_fitness
        .scenarios
        .iter()
        .filter(|s| {
            let rate = s.pass_rate();
            rate > 0.0 && rate < 1.0
        })
        .collect();
    let _ = writeln!(out, "\n-- Per-model consistency (mixed results only) --");
    if mixed.is_empty() {
        let _ = writeln!(out, "none — every scenario agreed across all models/samples");
    } else {
        for s in &mixed {
            let _ = writeln!(out, "{}: {}", s.name, s.trial_breakdown());
        }
    }

    if let Some(text) = justification {
        let _ = writeln!(out, "\n-- Justification (from the tuning model) --\n{text}");
    } else {
        let _ = writeln!(
            out,
            "\n-- Justification --\n(unavailable — call budget was exhausted before this could be requested)"
        );
    }

    let _ = writeln!(out, "\n-- Candidate prompt --\n--- BEGIN ---\n{}\n--- END ---", winner.prompt);
    let _ = writeln!(
        out,
        "\n-- Baseline prompt --\n--- BEGIN ---\n{baseline_prompt}\n--- END ---"
    );

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_eval::ScenarioOutcome;
    use crate::scoring::{ScenarioTrial, ScoredScenario};

    fn scored(name: &'static str, routed_correctly: bool) -> ScoredScenario {
        ScoredScenario {
            name,
            goal: "goal",
            expected: "Clarify",
            note: "note",
            trials: vec![ScenarioTrial {
                model: "test-model".to_string(),
                outcome: ScenarioOutcome {
                    routed_correctly,
                    safe_default_hit: None,
                    unsafe_act: false,
                },
            }],
        }
    }

    fn fitness(accuracy: f32, safe_default_rate: f32, unsafe_acts: usize, scenarios: Vec<ScoredScenario>) -> CandidateFitness {
        CandidateFitness {
            accuracy,
            safe_default_rate,
            unsafe_acts,
            scenarios,
        }
    }

    #[test]
    fn rubric_reports_cold_start_origin() {
        let winner = Candidate {
            prompt: "new prompt".into(),
            origin: CandidateOrigin::ColdStart,
        };
        let text = format_rubric(
            &winner,
            &fitness(0.9, 1.0, 0, vec![]),
            &fitness(0.8, 1.0, 0, vec![]),
            None,
        );
        assert!(text.contains("cold start"));
        assert!(text.contains("0.80 -> 0.90"));
    }

    #[test]
    fn rubric_reports_mutation_origin_with_parent_accuracy() {
        let winner = Candidate {
            prompt: "new prompt".into(),
            origin: CandidateOrigin::MutatedFrom {
                parent_index: 1,
                parent_accuracy: 0.75,
            },
        };
        let text = format_rubric(&winner, &fitness(0.9, 1.0, 0, vec![]), &fitness(0.8, 1.0, 0, vec![]), None);
        assert!(text.contains("beam candidate #1"));
        assert!(text.contains("0.75"));
    }

    #[test]
    fn rubric_names_newly_passing_and_newly_failing_scenarios() {
        let baseline = fitness(
            0.5,
            1.0,
            0,
            vec![scored("fixed_by_winner", false), scored("stays_passing", true)],
        );
        let winner_fit = fitness(
            0.5,
            1.0,
            0,
            vec![scored("fixed_by_winner", true), scored("stays_passing", false)],
        );
        let winner = Candidate {
            prompt: "p".into(),
            origin: CandidateOrigin::ColdStart,
        };
        let text = format_rubric(&winner, &winner_fit, &baseline, None);
        assert!(text.contains("fixed_by_winner"));
        assert!(text.contains("stays_passing"));
        assert!(text.contains("REGRESSION"));
    }

    #[test]
    fn rubric_includes_justification_when_present() {
        let winner = Candidate {
            prompt: "p".into(),
            origin: CandidateOrigin::ColdStart,
        };
        let text = format_rubric(
            &winner,
            &fitness(0.9, 1.0, 0, vec![]),
            &fitness(0.8, 1.0, 0, vec![]),
            Some("because it generalizes well"),
        );
        assert!(text.contains("because it generalizes well"));
    }

    #[test]
    fn rubric_notes_missing_justification() {
        let winner = Candidate {
            prompt: "p".into(),
            origin: CandidateOrigin::ColdStart,
        };
        let text = format_rubric(&winner, &fitness(0.9, 1.0, 0, vec![]), &fitness(0.8, 1.0, 0, vec![]), None);
        assert!(text.contains("unavailable"));
    }

    #[test]
    fn rubric_includes_both_prompts() {
        let winner = Candidate {
            prompt: "THE WINNING PROMPT TEXT".into(),
            origin: CandidateOrigin::ColdStart,
        };
        let text = format_rubric(&winner, &fitness(0.9, 1.0, 0, vec![]), &fitness(0.8, 1.0, 0, vec![]), None);
        assert!(text.contains("THE WINNING PROMPT TEXT"));
        assert!(text.contains(DEFAULT_SYSTEM_PROMPT));
    }

    #[test]
    fn rubric_shows_per_model_breakdown_only_for_mixed_scenarios() {
        let mixed = ScoredScenario {
            name: "mixed-scenario",
            goal: "goal",
            expected: "Clarify",
            note: "note",
            trials: vec![
                ScenarioTrial {
                    model: "deepseek".to_string(),
                    outcome: ScenarioOutcome { routed_correctly: true, safe_default_hit: None, unsafe_act: false },
                },
                ScenarioTrial {
                    model: "claude-haiku".to_string(),
                    outcome: ScenarioOutcome { routed_correctly: false, safe_default_hit: None, unsafe_act: false },
                },
            ],
        };
        let consistent = scored("consistent-scenario", true);

        let winner = Candidate { prompt: "p".into(), origin: CandidateOrigin::ColdStart };
        let winner_fit = fitness(0.75, 1.0, 0, vec![mixed, consistent]);
        let text = format_rubric(&winner, &winner_fit, &fitness(0.5, 1.0, 0, vec![]), None);

        assert!(text.contains("mixed-scenario: deepseek: 1/1 correct, claude-haiku: 0/1 correct"));
        assert!(!text.contains("consistent-scenario:"));
    }

    fn tool_loop_trial(model: &str, calls_matched: bool, unsafe_call: bool, outcome_matched: bool) -> crate::tool_loop_scoring::ToolLoopTrial {
        crate::tool_loop_scoring::ToolLoopTrial {
            model: model.to_string(),
            outcome: crate::tool_loop_scoring::ToolLoopOutcome {
                calls_matched,
                unsafe_call,
                outcome_matched,
            },
        }
    }

    fn tool_loop_scored(name: &'static str, calls_matched: bool) -> crate::tool_loop_scoring::ToolLoopScoredScenario {
        crate::tool_loop_scoring::ToolLoopScoredScenario {
            name,
            goal: "goal",
            note: "note",
            expect: crate::tool_scenarios::ToolLoopExpect {
                must_call: &[],
                must_not_call: &[],
                expected_outcome: liberado_common::Outcome::Succeeded,
            },
            trials: vec![tool_loop_trial("test-model", calls_matched, false, true)],
        }
    }

    fn tool_loop_fitness(
        accuracy: f32,
        outcome_match_rate: f32,
        unsafe_acts: usize,
        scenarios: Vec<crate::tool_loop_scoring::ToolLoopScoredScenario>,
    ) -> ToolLoopFitness {
        ToolLoopFitness {
            accuracy,
            outcome_match_rate,
            unsafe_acts,
            scenarios,
        }
    }

    #[test]
    fn executor_rubric_reports_deltas_and_baseline_prompt() {
        let winner = Candidate {
            prompt: "new executor prompt".into(),
            origin: CandidateOrigin::ColdStart,
        };
        let text = format_executor_rubric(
            &winner,
            &tool_loop_fitness(0.9, 0.95, 0, vec![]),
            &tool_loop_fitness(0.7, 0.8, 0, vec![]),
            "the baseline executor prompt",
            None,
        );
        assert!(text.contains("0.70 -> 0.90"));
        assert!(text.contains("0.80 -> 0.95"));
        assert!(text.contains("new executor prompt"));
        assert!(text.contains("the baseline executor prompt"));
        assert!(text.contains("unavailable"));
    }

    #[test]
    fn executor_rubric_shows_full_scenario_breakdown_unconditionally() {
        // Even with a single trial per scenario (no diff, no mixed results possible), the full
        // breakdown must still show exactly which scenario is behind a given accuracy number.
        let passing = tool_loop_scored("single-lookup", true);
        let failing = tool_loop_scored("multi-step-research", false);
        let winner = Candidate { prompt: "p".into(), origin: CandidateOrigin::ColdStart };
        let winner_fit = tool_loop_fitness(0.5, 1.0, 0, vec![passing, failing]);
        let text = format_executor_rubric(&winner, &winner_fit, &winner_fit, "baseline", None);
        assert!(text.contains("single-lookup: pass_rate=1.00"));
        assert!(text.contains("multi-step-research: pass_rate=0.00"));
        assert!(text.contains("calls matched"));
    }

    #[test]
    fn executor_rubric_names_regressions() {
        let baseline = tool_loop_fitness(0.5, 1.0, 0, vec![tool_loop_scored("a", false), tool_loop_scored("b", true)]);
        let winner_fit = tool_loop_fitness(0.5, 1.0, 0, vec![tool_loop_scored("a", true), tool_loop_scored("b", false)]);
        let winner = Candidate { prompt: "p".into(), origin: CandidateOrigin::ColdStart };
        let text = format_executor_rubric(&winner, &winner_fit, &baseline, "baseline", None);
        assert!(text.contains("- a"));
        assert!(text.contains("REGRESSION"));
        assert!(text.contains("- b"));
    }

    #[test]
    fn executor_rubric_shows_per_model_breakdown_only_for_mixed_scenarios() {
        let mixed = crate::tool_loop_scoring::ToolLoopScoredScenario {
            name: "mixed-scenario",
            goal: "goal",
            note: "note",
            expect: crate::tool_scenarios::ToolLoopExpect {
                must_call: &[],
                must_not_call: &[],
                expected_outcome: liberado_common::Outcome::Succeeded,
            },
            trials: vec![
                tool_loop_trial("deepseek", true, false, true),
                tool_loop_trial("claude-haiku", false, false, true),
            ],
        };
        let consistent = tool_loop_scored("consistent-scenario", true);
        let winner = Candidate { prompt: "p".into(), origin: CandidateOrigin::ColdStart };
        let winner_fit = tool_loop_fitness(0.75, 1.0, 0, vec![mixed, consistent]);
        let text = format_executor_rubric(&winner, &winner_fit, &tool_loop_fitness(0.5, 1.0, 0, vec![]), "baseline", None);
        assert!(text.contains("mixed-scenario: deepseek: 1/1 correct, claude-haiku: 0/1 correct"));
        // The full-scenario-breakdown section (added separately, unconditional) legitimately
        // mentions every scenario by name, so scope this assertion to the per-model-consistency
        // section specifically rather than the whole rubric.
        let consistency_section = text.split("-- Per-model consistency").nth(1).unwrap();
        assert!(!consistency_section.contains("consistent-scenario:"));
    }
}
