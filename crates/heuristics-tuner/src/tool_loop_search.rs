//! The executor/subagent-layer generation loop — deliberately parallel to [`crate::search`]'s
//! dispatcher-tuning loop rather than a generalization of it: see
//! `docs/future-work/heuristics-tuning-engine-plan.md`'s executor/subagent tuning extension for why
//! duplicating `select_beam`/`advance_beam`'s ~40 lines was the accepted tradeoff when this was
//! added (kept the addition fully additive, with zero risk of destabilizing the dispatcher path's
//! already-fixed elitism logic while this one was still new and unproven). Split into its own
//! module (2026-07-07, `docs/future-work/archive/hygiene-audit-2026-07-04.md`'s Priority 2 recommendation) once
//! both paths were proven live, so a reader no longer has to tell dispatcher-tuning and
//! executor-tuning code apart within one flat file.

use liberado_orchestrator::{DIRECT_INSTRUCTIONS, DIRECT_MAX_TURNS, SUBAGENT_PREAMBLE};

use crate::candidate::{Candidate, CandidateOrigin};
use crate::config::TunerConfig;
use crate::rubric::format_executor_rubric;
use crate::search::{Budget, request_justification_if_budget_allows};
use crate::tool_loop_generation::{cold_start_executor, mutate_executor};
use crate::tool_loop_scoring::{ToolLoopFitness, score_executor_candidate};

/// The executor-layer analog of [`crate::search::select_beam`] — same disqualify-then-rank logic,
/// but reading [`ToolLoopFitness`]'s fields instead of the dispatcher's.
pub fn select_beam_executor(
    scored: &[(Candidate, ToolLoopFitness)],
    beam_width: usize,
) -> Vec<usize> {
    let mut qualified: Vec<usize> = scored
        .iter()
        .enumerate()
        .filter(|(_, (_, fitness))| fitness.unsafe_acts == 0)
        .map(|(i, _)| i)
        .collect();

    qualified.sort_by(|&a, &b| {
        let fa = &scored[a].1;
        let fb = &scored[b].1;
        fb.accuracy
            .partial_cmp(&fa.accuracy)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                fb.outcome_match_rate
                    .partial_cmp(&fa.outcome_match_rate)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });

    qualified.truncate(beam_width);
    qualified
}

/// The executor-layer analog of `crate::search::advance_beam` — same elitism (the incumbent beam
/// is included in the same selection as the new pool, so a generation can never regress it).
fn advance_beam_executor(
    beam: &[(Candidate, ToolLoopFitness)],
    pool: Vec<(Candidate, ToolLoopFitness)>,
    beam_width: usize,
) -> Vec<(Candidate, ToolLoopFitness)> {
    let mut scored: Vec<(Candidate, ToolLoopFitness)> = beam.to_vec();
    scored.extend(pool);

    let survivors = select_beam_executor(&scored, beam_width);
    if survivors.is_empty() {
        beam.to_vec()
    } else {
        survivors
            .into_iter()
            .map(|idx| scored[idx].clone())
            .collect()
    }
}

/// The executor-layer analog of [`crate::search::GenerationRecord`].
pub struct ExecutorGenerationRecord {
    pub generation: usize,
    pub candidate: Candidate,
    pub fitness: ToolLoopFitness,
    pub rubric: String,
}

/// The executor-layer analog of [`crate::search::TunerResult`].
pub struct ExecutorTunerResult {
    pub winner: Candidate,
    pub winner_fitness: ToolLoopFitness,
    pub baseline_fitness: ToolLoopFitness,
    pub rubric: String,
    pub generations: Vec<ExecutorGenerationRecord>,
}

/// The executor-layer analog of [`crate::search::run_tuner`]: same beam-search-with-restarts loop
/// shape, scored by `score_executor_candidate` (a real, mocked `Executor::execute` tool loop per
/// trial) instead of a single dispatcher classification call. Seeded from `DIRECT_INSTRUCTIONS`
/// (`liberado_orchestrator::DIRECT_MAX_TURNS` turn budget).
pub async fn run_executor_tuner(config: TunerConfig) -> ExecutorTunerResult {
    run_tool_loop_tuner(config, DIRECT_INSTRUCTIONS, DIRECT_MAX_TURNS).await
}

/// The subagent-layer counterpart of [`run_executor_tuner`] — identical machinery (both roles run
/// through `liberado_executor::Executor::execute`, which is what `select_beam_executor`/
/// `advance_beam_executor`/`score_executor_candidate` actually operate on), seeded from
/// `SUBAGENT_PREAMBLE` instead, with the subagent's own (looser) turn budget
/// (`liberado_executor::DEFAULT_MAX_TURNS` — mirrors `Orchestrator`'s `subagent_budget` default,
/// distinct from the executor's tighter `DIRECT_MAX_TURNS`).
pub async fn run_subagent_tuner(config: TunerConfig) -> ExecutorTunerResult {
    run_tool_loop_tuner(
        config,
        SUBAGENT_PREAMBLE,
        liberado_executor::DEFAULT_MAX_TURNS,
    )
    .await
}

/// The final result's rubric for the executor/subagent loop: reuse the winner's last generation
/// record (same candidate, already formatted against the baseline) or format a fresh one when no
/// generation finished. Pure — no model call — so the reuse-vs-fallback decision is directly testable.
fn finalize_result_executor(
    winner: &Candidate,
    winner_fitness: &ToolLoopFitness,
    baseline_fitness: &ToolLoopFitness,
    seed_prompt: &str,
    generations: &[ExecutorGenerationRecord],
) -> String {
    generations
        .last()
        .map(|g| g.rubric.clone())
        .unwrap_or_else(|| {
            format_executor_rubric(winner, winner_fitness, baseline_fitness, seed_prompt, None)
        })
}

/// Produce one generation's candidate pool: mutations from every beam parent plus independent cold
/// starts, halting on the shared budget. The only model-bound part of a generation — the loop and
/// budget bookkeeping live here so `run_tool_loop_tuner`'s driver reads as a flat sequence of decisions.
async fn gather_generation_candidates_executor(
    beam: &[(Candidate, ToolLoopFitness)],
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
            if let Ok(prompt) = mutate_executor(
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
                        parent_accuracy: parent_fitness.accuracy,
                    },
                });
            } // Err logged inside mutate_executor(); skip this slot, not the run
        }
    }

    for _ in 0..config.cold_starts_per_generation {
        if budget.exhausted() {
            break;
        }
        if let Ok(prompt) = cold_start_executor(config.meta_provider.as_ref(), budget).await {
            pool.push(Candidate {
                prompt,
                origin: CandidateOrigin::ColdStart,
            });
        }
    }

    pool
}

/// Score every candidate in a pool against the executor scenario set — a tight loop whose only work
/// is waiting on the shared scoring calls. Kept separate so the beam advance that consumes its
/// output reads as a decision, and so the per-generation driver stays free of the loop shape.
async fn score_pool_executor(
    pool: Vec<Candidate>,
    config: &TunerConfig,
    budget: &Budget,
    max_turns: u32,
) -> Vec<(Candidate, ToolLoopFitness)> {
    let mut scored = Vec::with_capacity(pool.len());
    for candidate in pool {
        let fitness = score_executor_candidate(
            &candidate.prompt,
            &config.scoring_providers,
            config.samples_per_scenario,
            config.max_scenarios,
            max_turns,
            budget,
        )
        .await;
        scored.push((candidate, fitness));
    }
    scored
}

/// Shared beam-search loop for any role that runs through `Executor::execute` (today: executor and
/// subagent) — the two public entry points above differ only in seed prompt and turn budget.
async fn run_tool_loop_tuner(
    config: TunerConfig,
    seed_prompt: &str,
    max_turns: u32,
) -> ExecutorTunerResult {
    let budget = Budget::new(config.call_budget);

    let baseline_fitness = score_executor_candidate(
        seed_prompt,
        &config.scoring_providers,
        config.samples_per_scenario,
        config.max_scenarios,
        max_turns,
        &budget,
    )
    .await;
    let baseline = Candidate {
        prompt: seed_prompt.to_string(),
        origin: CandidateOrigin::ColdStart,
    };

    let mut beam: Vec<(Candidate, ToolLoopFitness)> = vec![(baseline, baseline_fitness.clone())];
    let mut generations: Vec<ExecutorGenerationRecord> = Vec::new();

    for generation_index in 0..config.max_generations {
        if budget.exhausted() {
            break;
        }
        let pool = gather_generation_candidates_executor(&beam, &config, &budget).await;
        if pool.is_empty() {
            break; // budget ran out before a single candidate could be produced this generation
        }
        let scored = score_pool_executor(pool, &config, &budget, max_turns).await;
        beam = advance_beam_executor(&beam, scored, config.beam_width);

        let (best_candidate, best_fitness) = &beam[0];
        let justification = request_justification_if_budget_allows(
            config.meta_provider.as_ref(),
            &best_candidate.prompt,
            &budget,
        )
        .await;
        let rubric = format_executor_rubric(
            best_candidate,
            best_fitness,
            &baseline_fitness,
            seed_prompt,
            justification.as_deref(),
        );
        generations.push(ExecutorGenerationRecord {
            generation: generation_index + 1,
            candidate: best_candidate.clone(),
            fitness: best_fitness.clone(),
            rubric,
        });
    }

    let (winner, winner_fitness) = beam.into_iter().next().expect("beam is never empty");
    let rubric = finalize_result_executor(
        &winner,
        &winner_fitness,
        &baseline_fitness,
        seed_prompt,
        &generations,
    );

    ExecutorTunerResult {
        winner,
        winner_fitness,
        baseline_fitness,
        rubric,
        generations,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::candidate::CandidateOrigin;

    fn candidate(prompt: &str) -> Candidate {
        Candidate {
            prompt: prompt.to_string(),
            origin: CandidateOrigin::ColdStart,
        }
    }

    fn tool_loop_fitness(
        accuracy: f32,
        outcome_match_rate: f32,
        unsafe_acts: usize,
    ) -> ToolLoopFitness {
        ToolLoopFitness {
            accuracy,
            outcome_match_rate,
            unsafe_acts,
            scenarios: Vec::new(),
        }
    }

    #[test]
    fn select_beam_executor_excludes_unsafe_candidates_even_at_top_accuracy() {
        let scored = vec![
            (
                candidate("unsafe-but-accurate"),
                tool_loop_fitness(0.95, 1.0, 1),
            ),
            (
                candidate("safe-but-less-accurate"),
                tool_loop_fitness(0.80, 1.0, 0),
            ),
        ];
        assert_eq!(select_beam_executor(&scored, 2), vec![1]);
    }

    #[test]
    fn select_beam_executor_orders_by_accuracy_then_outcome_match_rate() {
        let scored = vec![
            (candidate("a"), tool_loop_fitness(0.80, 0.5, 0)),
            (candidate("b"), tool_loop_fitness(0.90, 1.0, 0)),
            (candidate("c"), tool_loop_fitness(0.90, 0.5, 0)),
        ];
        assert_eq!(select_beam_executor(&scored, 3), vec![1, 2, 0]);
    }

    #[test]
    fn advance_beam_executor_never_regresses_below_a_safe_incumbent() {
        let beam = vec![(candidate("incumbent"), tool_loop_fitness(0.77, 1.0, 0))];
        let pool = vec![(
            candidate("regressive-cold-start"),
            tool_loop_fitness(0.33, 1.0, 0),
        )];
        let next = advance_beam_executor(&beam, pool, 1);
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].0.prompt, "incumbent");
    }

    #[test]
    fn advance_beam_executor_adopts_a_genuinely_better_new_candidate() {
        let beam = vec![(candidate("incumbent"), tool_loop_fitness(0.77, 1.0, 0))];
        let pool = vec![(
            candidate("improved-mutation"),
            tool_loop_fitness(0.90, 1.0, 0),
        )];
        let next = advance_beam_executor(&beam, pool, 1);
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].0.prompt, "improved-mutation");
    }

    #[test]
    fn advance_beam_executor_falls_back_to_incumbent_when_everything_is_disqualified() {
        let beam = vec![(
            candidate("unsafe-incumbent"),
            tool_loop_fitness(0.9, 1.0, 1),
        )];
        let pool = vec![(candidate("also-unsafe"), tool_loop_fitness(0.5, 1.0, 2))];
        let next = advance_beam_executor(&beam, pool, 1);
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].0.prompt, "unsafe-incumbent");
    }

    #[tokio::test]
    #[ignore = "requires OPENROUTER_API_KEY + network access"]
    async fn live_end_to_end_executor() {
        // SAFETY: single-threaded test process assumption — these vars aren't read by any other
        // concurrently-running test, only by TunerConfig::load() called immediately after.
        unsafe {
            std::env::set_var("TUNER_LAYER", "executor");
            std::env::set_var("TUNER_MAX_GENERATIONS", "1");
            std::env::set_var("TUNER_MUTATIONS_PER_CANDIDATE", "1");
            std::env::set_var("TUNER_COLD_STARTS_PER_GENERATION", "1");
            std::env::set_var("TUNER_BEAM_WIDTH", "1");
            std::env::set_var("TUNER_CALL_BUDGET", "60");
        }
        let config = TunerConfig::load().expect("OPENROUTER_API_KEY not set");
        assert_eq!(config.layer, crate::config::Layer::Executor);
        let result = run_executor_tuner(config).await;
        assert!(!result.rubric.is_empty());
        assert_eq!(
            result.winner_fitness.unsafe_acts, 0,
            "a winner must never carry an unsafe act"
        );
    }

    #[tokio::test]
    #[ignore = "requires OPENROUTER_API_KEY + network access"]
    async fn live_end_to_end_subagent() {
        // Mirrors `live_end_to_end_executor`, but via TUNER_LAYER=subagent — same underlying
        // `run_tool_loop_tuner`, seeded from SUBAGENT_PREAMBLE with the looser subagent turn budget.
        unsafe {
            std::env::set_var("TUNER_LAYER", "subagent");
            std::env::set_var("TUNER_MAX_GENERATIONS", "1");
            std::env::set_var("TUNER_MUTATIONS_PER_CANDIDATE", "1");
            std::env::set_var("TUNER_COLD_STARTS_PER_GENERATION", "1");
            std::env::set_var("TUNER_BEAM_WIDTH", "1");
            std::env::set_var("TUNER_CALL_BUDGET", "60");
        }
        let config = TunerConfig::load().expect("OPENROUTER_API_KEY not set");
        assert_eq!(config.layer, crate::config::Layer::Subagent);
        let result = run_subagent_tuner(config).await;
        assert!(!result.rubric.is_empty());
        assert_eq!(
            result.winner_fitness.unsafe_acts, 0,
            "a winner must never carry an unsafe act"
        );
    }

    #[test]
    fn finalize_result_executor_reuses_the_last_generation_rubric() {
        let winner = candidate("best");
        let winner_fit = tool_loop_fitness(0.9, 1.0, 0);
        let baseline = tool_loop_fitness(0.5, 1.0, 0);
        let generations = vec![ExecutorGenerationRecord {
            generation: 1,
            candidate: winner.clone(),
            fitness: winner_fit.clone(),
            rubric: "already-the-winner".to_string(),
        }];
        // Non-empty generations: the last record (same candidate) is reused verbatim.
        assert_eq!(
            finalize_result_executor(&winner, &winner_fit, &baseline, "seed", &generations),
            "already-the-winner",
        );
    }

    #[test]
    fn finalize_result_executor_formats_fresh_when_no_generation_finished() {
        let winner = candidate("best");
        let winner_fit = tool_loop_fitness(0.9, 1.0, 0);
        let baseline = tool_loop_fitness(0.5, 1.0, 0);
        let rubric = finalize_result_executor(&winner, &winner_fit, &baseline, "seed", &[]);
        assert!(
            rubric.contains("Heuristics Tuner Proposal"),
            "a fresh executor rubric formats on the empty-generations path"
        );
    }
}
