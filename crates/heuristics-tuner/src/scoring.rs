//! Score a candidate system prompt against `liberado-eval`'s existing scenario set — the
//! dispatcher-layer v1 scope (`docs/roadmap/heuristics-tuning-engine-plan.md`). No tool execution
//! happens anywhere in this module: scoring is purely "did `Dispatcher::dispatch` pick the right
//! action," reusing `liberado_eval::score`'s classification rules rather than re-deriving them.

use std::sync::Arc;

use liberado_common::{Capability, CapabilitySet};
use liberado_dispatcher::{DispatchRequest, Dispatcher, McpDescriptor};
use liberado_eval::{Scenario, ScenarioOutcome, scenarios};
use liberado_provider::Provider;

use crate::search::Budget;

/// One scenario's real-model outcome, with enough context (`expected`/`got`/`note`) for the
/// mutation prompt to explain a failure to the tuning model without a second lookup.
#[derive(Debug, Clone)]
pub struct ScoredScenario {
    pub name: &'static str,
    pub goal: &'static str,
    pub expected: &'static str,
    pub got: String,
    pub note: &'static str,
    pub outcome: ScenarioOutcome,
}

/// How a candidate prompt performed across the whole scenario set.
#[derive(Debug, Clone)]
pub struct CandidateFitness {
    pub accuracy: f32,
    /// Fraction of `Clarify`-expected scenarios that got a safe default (`Clarify` or `Propose`).
    /// `1.0` when no scenario expects `Clarify` (vacuously safe — nothing to have gotten wrong).
    pub safe_default_rate: f32,
    /// The hard gate: a candidate with `unsafe_acts > 0` is disqualified outright by the search
    /// loop, not merely scored lower (see `search::run_tuner`).
    pub unsafe_acts: usize,
    pub scenarios: Vec<ScoredScenario>,
}

impl CandidateFitness {
    /// Names of scenarios this candidate got wrong — the mutation prompt's input.
    pub fn failing(&self) -> Vec<&ScoredScenario> {
        self.scenarios
            .iter()
            .filter(|s| !s.outcome.routed_correctly)
            .collect()
    }
}

/// Fold per-scenario outcomes into a candidate's overall fitness. Pure — the directly
/// unit-testable half of scoring.
pub fn aggregate(scenarios: Vec<ScoredScenario>) -> CandidateFitness {
    let total = scenarios.len().max(1);
    let correct = scenarios.iter().filter(|s| s.outcome.routed_correctly).count();

    let clarify_expected = scenarios
        .iter()
        .filter(|s| s.outcome.safe_default_hit.is_some())
        .count();
    let clarify_hit = scenarios
        .iter()
        .filter(|s| s.outcome.safe_default_hit == Some(true))
        .count();
    let unsafe_acts = scenarios.iter().filter(|s| s.outcome.unsafe_act).count();

    let safe_default_rate = if clarify_expected == 0 {
        1.0
    } else {
        clarify_hit as f32 / clarify_expected as f32
    };

    CandidateFitness {
        accuracy: correct as f32 / total as f32,
        safe_default_rate,
        unsafe_acts,
        scenarios,
    }
}

/// Score `prompt` against every scenario in `liberado_eval::scenarios()`, concurrently, against
/// `scoring_provider` (which plays "the real dispatcher" for this run — see `TunerConfig` for why
/// it defaults to the production model). Each scenario call is charged against `budget`; a
/// scenario is skipped once the budget is exhausted, so a mid-run cutoff still yields a (partial,
/// honestly-reported) fitness rather than panicking.
pub async fn score_candidate(
    prompt: &str,
    scoring_provider: Arc<dyn Provider>,
    budget: &Budget,
) -> CandidateFitness {
    let dispatcher = Arc::new(
        Dispatcher::new(
            scoring_provider,
            liberado_common::config::DispatchTuning::default(),
            liberado_common::config::ConcurrencyTuning::default().max_reaction_depth,
        )
        .with_system_prompt(prompt),
    );

    let mut set = tokio::task::JoinSet::new();
    for scenario in scenarios() {
        if !budget.spend() {
            break;
        }
        let dispatcher = Arc::clone(&dispatcher);
        set.spawn(async move { score_one(&dispatcher, scenario).await });
    }

    let mut scored = Vec::new();
    while let Some(res) = set.join_next().await {
        if let Ok(Some(s)) = res {
            scored.push(s);
        }
    }
    aggregate(scored)
}

async fn score_one(dispatcher: &Dispatcher, scenario: Scenario) -> Option<ScoredScenario> {
    let request = DispatchRequest {
        goal: scenario.goal.to_string(),
        catalog: scenario
            .catalog
            .iter()
            .map(|(name, desc, consequence)| McpDescriptor {
                name: name.to_string(),
                description: desc.to_string(),
                consequence: *consequence,
                provenance: None,
            })
            .collect(),
        capabilities: CapabilitySet::from_iter(
            scenario
                .granted
                .iter()
                .map(|n| Capability::ExecuteMcp(n.to_string())),
        ),
        reaction_depth: 0,
    };

    let decision = dispatcher.dispatch(&request).await.ok()?;
    let outcome = liberado_eval::score(&scenario, &decision);
    Some(ScoredScenario {
        name: scenario.name,
        goal: scenario.goal,
        expected: scenario.expect.label(),
        got: decision.action.label().to_string(),
        note: scenario.note,
        outcome,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scored(name: &'static str, routed_correctly: bool, safe_default_hit: Option<bool>, unsafe_act: bool) -> ScoredScenario {
        ScoredScenario {
            name,
            goal: "goal",
            expected: "Clarify",
            got: "ExecuteDirect".into(),
            note: "note",
            outcome: ScenarioOutcome {
                routed_correctly,
                safe_default_hit,
                unsafe_act,
            },
        }
    }

    #[test]
    fn accuracy_is_fraction_correct() {
        let fitness = aggregate(vec![
            scored("a", true, None, false),
            scored("b", true, None, false),
            scored("c", false, None, false),
            scored("d", false, None, false),
        ]);
        assert_eq!(fitness.accuracy, 0.5);
    }

    #[test]
    fn safe_default_rate_is_vacuously_one_with_no_clarify_scenarios() {
        let fitness = aggregate(vec![scored("a", true, None, false)]);
        assert_eq!(fitness.safe_default_rate, 1.0);
    }

    #[test]
    fn safe_default_rate_counts_only_clarify_expected_scenarios() {
        let fitness = aggregate(vec![
            scored("a", true, Some(true), false),
            scored("b", false, Some(false), true),
            scored("c", true, None, false), // not a Clarify scenario -- excluded from the rate
        ]);
        assert_eq!(fitness.safe_default_rate, 0.5);
        assert_eq!(fitness.unsafe_acts, 1);
    }

    #[test]
    fn unsafe_acts_counts_regardless_of_overall_accuracy() {
        // A candidate could score high accuracy overall while still committing an unsafe act on
        // one scenario -- the hard gate must not get diluted by the aggregate.
        let fitness = aggregate(vec![
            scored("a", true, None, false),
            scored("b", true, None, false),
            scored("c", true, None, false),
            scored("d", false, Some(false), true),
        ]);
        assert_eq!(fitness.accuracy, 0.75);
        assert_eq!(fitness.unsafe_acts, 1);
    }

    #[test]
    fn failing_returns_only_incorrect_scenarios() {
        let fitness = aggregate(vec![
            scored("a", true, None, false),
            scored("b", false, None, false),
        ]);
        let failing = fitness.failing();
        assert_eq!(failing.len(), 1);
        assert_eq!(failing[0].name, "b");
    }

    #[test]
    fn empty_scenario_set_does_not_divide_by_zero() {
        let fitness = aggregate(vec![]);
        assert_eq!(fitness.accuracy, 0.0);
        assert_eq!(fitness.safe_default_rate, 1.0);
        assert_eq!(fitness.unsafe_acts, 0);
    }
}
