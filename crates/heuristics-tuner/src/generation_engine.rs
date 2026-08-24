//! The generation half of one beam-search generation, written once.
//!
//! Dispatcher, coder, and executor tuning each carried a byte-identical
//! `gather_generation_candidates*` that differed only in which fitness type
//! and which `mutate`/`cold_start` pair it called. The loop below is the
//! single implementation; each domain supplies a zero-size adapter plus a
//! [`GenerationFitness`] impl, and keeps a thin same-named wrapper so its
//! search file reads exactly as before.

use liberado_provider::Provider;

use crate::candidate::{Candidate, CandidateOrigin};
use crate::coder_generation::{cold_start_coder, mutate_coder};
use crate::coder_scoring::CoderFitness;
use crate::coder_scoring::CoderScoredScenario;
use crate::config::TunerConfig;
use crate::generation::{GenerationError, cold_start, mutate};
use crate::scoring::{CandidateFitness, ScoredScenario};
use crate::search::Budget;
use crate::tool_loop_generation::{cold_start_executor, mutate_executor};
use crate::tool_loop_scoring::{ToolLoopFitness, ToolLoopScoredScenario};

/// What the generation engine reads off a domain's fitness value.
///
/// Inherent methods of the same name stay authoritative at existing call
/// sites; these impls only let the generic loop see them.
pub(crate) trait GenerationFitness {
    /// The scored-scenario type this fitness carries.
    type Scenario: Sync;
    /// Mean accuracy across scenarios.
    fn accuracy(&self) -> f32;
    /// Scenarios the candidate got wrong on balance - the mutation prompt's
    /// failure context.
    fn failing(&self) -> Vec<&Self::Scenario>;
}

/// A domain's prompt-generation pair (`mutate` + `cold_start`).
pub(crate) trait DomainGeneration {
    /// The scenario type this domain's mutate consumes.
    type Scenario: Sync;
    /// The fitness this domain's beam carries.
    type Fitness: GenerationFitness<Scenario = Self::Scenario>;

    fn mutate(
        &self,
        meta_provider: &dyn Provider,
        parent_prompt: &str,
        failing: &[&Self::Scenario],
        budget: &Budget,
    ) -> impl std::future::Future<Output = Result<String, GenerationError>> + Send;

    fn cold_start(
        &self,
        meta_provider: &dyn Provider,
        budget: &Budget,
    ) -> impl std::future::Future<Output = Result<String, GenerationError>> + Send;
}

/// Dispatcher-layer adapter.
pub(crate) struct DispatcherGeneration;

impl DomainGeneration for DispatcherGeneration {
    type Scenario = ScoredScenario;
    type Fitness = CandidateFitness;

    async fn mutate(
        &self,
        meta_provider: &dyn Provider,
        parent_prompt: &str,
        failing: &[&Self::Scenario],
        budget: &Budget,
    ) -> Result<String, GenerationError> {
        mutate(meta_provider, parent_prompt, failing, budget).await
    }

    async fn cold_start(
        &self,
        meta_provider: &dyn Provider,
        budget: &Budget,
    ) -> Result<String, GenerationError> {
        cold_start(meta_provider, budget).await
    }
}

impl GenerationFitness for CandidateFitness {
    type Scenario = ScoredScenario;

    fn accuracy(&self) -> f32 {
        self.accuracy
    }

    fn failing(&self) -> Vec<&ScoredScenario> {
        CandidateFitness::failing(self)
    }
}

/// Coder-layer adapter.
pub(crate) struct CoderGeneration;

impl DomainGeneration for CoderGeneration {
    type Scenario = CoderScoredScenario;
    type Fitness = CoderFitness;

    async fn mutate(
        &self,
        meta_provider: &dyn Provider,
        parent_prompt: &str,
        failing: &[&Self::Scenario],
        budget: &Budget,
    ) -> Result<String, GenerationError> {
        mutate_coder(meta_provider, parent_prompt, failing, budget).await
    }

    async fn cold_start(
        &self,
        meta_provider: &dyn Provider,
        budget: &Budget,
    ) -> Result<String, GenerationError> {
        cold_start_coder(meta_provider, budget).await
    }
}

impl GenerationFitness for CoderFitness {
    type Scenario = CoderScoredScenario;

    fn accuracy(&self) -> f32 {
        self.accuracy
    }

    fn failing(&self) -> Vec<&CoderScoredScenario> {
        CoderFitness::failing(self)
    }
}

/// Executor/tool-loop adapter.
pub(crate) struct ExecutorGeneration;

impl DomainGeneration for ExecutorGeneration {
    type Scenario = ToolLoopScoredScenario;
    type Fitness = ToolLoopFitness;

    async fn mutate(
        &self,
        meta_provider: &dyn Provider,
        parent_prompt: &str,
        failing: &[&Self::Scenario],
        budget: &Budget,
    ) -> Result<String, GenerationError> {
        mutate_executor(meta_provider, parent_prompt, failing, budget).await
    }

    async fn cold_start(
        &self,
        meta_provider: &dyn Provider,
        budget: &Budget,
    ) -> Result<String, GenerationError> {
        cold_start_executor(meta_provider, budget).await
    }
}

impl GenerationFitness for ToolLoopFitness {
    type Scenario = ToolLoopScoredScenario;

    fn accuracy(&self) -> f32 {
        self.accuracy
    }

    fn failing(&self) -> Vec<&ToolLoopScoredScenario> {
        ToolLoopFitness::failing(self)
    }
}

/// Build one generation's candidate pool from the current beam.
///
/// Mutated children inherit their parent's index and accuracy; exhausted
/// budget stops slot production without failing the run; a failed mutation
/// skips its slot, not the run (the error is logged inside `mutate`).
pub(crate) async fn gather_generation_candidates<G: DomainGeneration>(
    generation: G,
    beam: &[(Candidate, G::Fitness)],
    config: &TunerConfig,
    budget: &Budget,
) -> Vec<Candidate> {
    let mut pool: Vec<Candidate> = Vec::new();

    for (parent_index, (parent, parent_fitness)) in beam.iter().enumerate() {
        for _ in 0..config.mutations_per_candidate {
            if budget.exhausted() {
                break;
            }
            let failing = parent_fitness.failing();
            if let Ok(prompt) = generation
                .mutate(
                    config.meta_provider.as_ref(),
                    &parent.prompt,
                    &failing,
                    budget,
                )
                .await
            {
                pool.push(Candidate {
                    prompt,
                    origin: CandidateOrigin::MutatedFrom {
                        parent_index,
                        parent_accuracy: parent_fitness.accuracy(),
                    },
                });
            }
        }
    }

    for _ in 0..config.cold_starts_per_generation {
        if budget.exhausted() {
            break;
        }
        if let Ok(prompt) = generation
            .cold_start(config.meta_provider.as_ref(), budget)
            .await
        {
            pool.push(Candidate {
                prompt,
                origin: CandidateOrigin::ColdStart,
            });
        }
    }

    pool
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coder_scenarios::CoderTier;
    use crate::config::{Layer, TunerConfig};
    use liberado_provider::MockProvider;
    use std::sync::Arc;

    /// Scripted domain: mutations for prompts listed in `fail_for` error out
    /// (as if the meta-provider call failed); everything else succeeds, and
    /// cold starts always do. No provider is ever contacted.
    struct Scripted {
        fail_for: &'static [&'static str],
    }

    impl DomainGeneration for Scripted {
        type Scenario = ScoredScenario;
        type Fitness = CandidateFitness;

        async fn mutate(
            &self,
            _meta_provider: &dyn Provider,
            parent_prompt: &str,
            _failing: &[&Self::Scenario],
            _budget: &Budget,
        ) -> Result<String, GenerationError> {
            if self.fail_for.contains(&parent_prompt) {
                Err(GenerationError::BudgetExhausted)
            } else {
                Ok(format!("child of {parent_prompt}"))
            }
        }

        async fn cold_start(
            &self,
            _meta_provider: &dyn Provider,
            _budget: &Budget,
        ) -> Result<String, GenerationError> {
            Ok("cold".into())
        }
    }

    fn fitness(accuracy: f32) -> CandidateFitness {
        CandidateFitness {
            accuracy,
            safe_default_rate: 1.0,
            unsafe_acts: 0,
            scenarios: Vec::new(),
        }
    }

    fn config(mutations: usize, cold_starts: usize) -> TunerConfig {
        TunerConfig {
            layer: Layer::Dispatcher,
            scoring_providers: Vec::new(),
            meta_provider: Arc::new(MockProvider::new("mock")),
            samples_per_scenario: 1,
            max_scenarios: None,
            coder_tier: CoderTier::Smoke,
            coder_scenario_filter: None,
            beam_width: 2,
            cold_starts_per_generation: cold_starts,
            mutations_per_candidate: mutations,
            max_generations: 1,
            call_budget: 100,
        }
    }

    fn candidate(prompt: &str) -> Candidate {
        Candidate {
            prompt: prompt.to_string(),
            origin: CandidateOrigin::ColdStart,
        }
    }

    #[tokio::test]
    async fn children_carry_parent_index_and_accuracy() {
        let beam = vec![
            (candidate("p0"), fitness(0.25)),
            (candidate("p1"), fitness(0.75)),
        ];
        let pool = gather_generation_candidates(
            Scripted { fail_for: &[] },
            &beam,
            &config(2, 0),
            &Budget::new(50),
        )
        .await;
        let mutated: Vec<_> = pool
            .iter()
            .filter_map(|c| match c.origin {
                CandidateOrigin::MutatedFrom {
                    parent_index,
                    parent_accuracy,
                } => Some((parent_index, parent_accuracy)),
                _ => None,
            })
            .collect();
        assert_eq!(pool.len(), 4, "two slots per parent");
        assert_eq!(mutated.len(), 4);
        assert_eq!(mutated[0], (0, 0.25));
        assert_eq!(mutated[3], (1, 0.75));
    }

    #[tokio::test]
    async fn a_failed_mutation_skips_its_slot_not_the_run() {
        let beam = vec![(candidate("doomed"), fitness(0.5))];
        let pool = gather_generation_candidates(
            Scripted {
                fail_for: &["doomed"],
            },
            &beam,
            &config(3, 1),
            &Budget::new(50),
        )
        .await;
        // Every mutation slot failed; only the cold start survives.
        assert_eq!(pool.len(), 1, "{pool:?}");
        assert!(matches!(pool[0].origin, CandidateOrigin::ColdStart));
    }

    #[tokio::test]
    async fn exhausted_budget_stops_production_before_any_call() {
        let beam = vec![
            (candidate("a"), fitness(0.9)),
            (candidate("b"), fitness(0.9)),
        ];
        let budget = Budget::new(1);
        budget.spend();
        assert!(budget.exhausted(), "precondition");
        let pool =
            gather_generation_candidates(Scripted { fail_for: &[] }, &beam, &config(2, 2), &budget)
                .await;
        assert!(pool.is_empty(), "nothing may be produced: {pool:?}");
    }
}
