//! Beam-search loop for the **coder** layer — parallel to [`crate::tool_loop_search`], not a
//! generalization of it (same deliberate tradeoff as executor vs dispatcher).

use crate::candidate::{Candidate, CandidateOrigin};
use crate::coder_generation::{cold_start_coder, mutate_coder};
use crate::coder_scenarios::{CoderTier, DEFAULT_CODER_SYSTEM_PROMPT};
use crate::coder_scoring::{CoderFitness, score_coder_candidate};
use crate::config::TunerConfig;
use crate::rubric::format_coder_rubric;
use crate::search::{Budget, request_justification_if_budget_allows};

/// Default turn budget for a coding trial during scoring (stress needs more room).
pub const CODER_MAX_TURNS: u32 = 16;

fn max_turns_for_tier(tier: CoderTier) -> u32 {
    match tier {
        CoderTier::Smoke => 10,
        CoderTier::Core => 14,
        CoderTier::Stress => 20,
        CoderTier::Greenfield => 28,
    }
}

pub fn select_beam_coder(scored: &[(Candidate, CoderFitness)], beam_width: usize) -> Vec<usize> {
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
                fb.nonempty_diff_rate
                    .partial_cmp(&fa.nonempty_diff_rate)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| {
                fb.outcome_match_rate
                    .partial_cmp(&fa.outcome_match_rate)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });

    qualified.truncate(beam_width);
    qualified
}

fn advance_beam_coder(
    beam: &[(Candidate, CoderFitness)],
    pool: Vec<(Candidate, CoderFitness)>,
    beam_width: usize,
) -> Vec<(Candidate, CoderFitness)> {
    let mut scored: Vec<(Candidate, CoderFitness)> = beam.to_vec();
    scored.extend(pool);
    let survivors = select_beam_coder(&scored, beam_width);
    if survivors.is_empty() {
        beam.to_vec()
    } else {
        survivors
            .into_iter()
            .map(|idx| scored[idx].clone())
            .collect()
    }
}

pub struct CoderGenerationRecord {
    pub generation: usize,
    pub candidate: Candidate,
    pub fitness: CoderFitness,
    pub rubric: String,
}

pub struct CoderTunerResult {
    pub winner: Candidate,
    pub winner_fitness: CoderFitness,
    pub baseline_fitness: CoderFitness,
    pub rubric: String,
    pub generations: Vec<CoderGenerationRecord>,
}

/// The final result's rubric for the coder tuner: reuse the last generation's record (same
/// candidate, already formatted against the baseline) or format a fresh one when no generation
/// finished. Pure — no model call — so the reuse-vs-fallback decision is directly testable.
fn finalize_result_coder(
    winner: &Candidate,
    winner_fitness: &CoderFitness,
    baseline_fitness: &CoderFitness,
    seed_prompt: &str,
    generations: &[CoderGenerationRecord],
) -> String {
    generations
        .last()
        .map(|g| g.rubric.clone())
        .unwrap_or_else(|| {
            format_coder_rubric(winner, winner_fitness, baseline_fitness, seed_prompt, None)
        })
}

/// Tune the Liberado coder role system prompt against real workspace coding scenarios.
pub async fn run_coder_tuner(config: TunerConfig) -> CoderTunerResult {
    let seed_prompt = DEFAULT_CODER_SYSTEM_PROMPT;
    let budget = Budget::new(config.call_budget);
    let tier = config.coder_tier;
    let max_turns = max_turns_for_tier(tier);
    tracing::info!(
        tier = %tier.as_str(),
        max_scenarios = ?config.max_scenarios,
        max_turns,
        "coder tuner curriculum"
    );

    let name_filter = config.coder_scenario_filter.as_deref();
    let baseline_fitness = score_coder_candidate(
        seed_prompt,
        &config.scoring_providers,
        config.samples_per_scenario,
        tier,
        config.max_scenarios,
        name_filter,
        max_turns,
        &budget,
    )
    .await;
    let baseline = Candidate {
        prompt: seed_prompt.to_string(),
        origin: CandidateOrigin::ColdStart,
    };

    let mut beam: Vec<(Candidate, CoderFitness)> = vec![(baseline, baseline_fitness.clone())];
    let mut generations: Vec<CoderGenerationRecord> = Vec::new();

    for generation_index in 0..config.max_generations {
        if budget.exhausted() {
            break;
        }

        let mut pool: Vec<Candidate> = Vec::new();

        for (parent_index, (parent, parent_fitness)) in beam.iter().enumerate() {
            for _ in 0..config.mutations_per_candidate {
                if budget.exhausted() {
                    break;
                }
                let failing = parent_fitness.failing();
                match mutate_coder(
                    config.meta_provider.as_ref(),
                    &parent.prompt,
                    &failing,
                    &budget,
                )
                .await
                {
                    Ok(prompt) => pool.push(Candidate {
                        prompt,
                        origin: CandidateOrigin::MutatedFrom {
                            parent_index,
                            parent_accuracy: parent_fitness.accuracy,
                        },
                    }),
                    Err(_) => continue,
                }
            }
        }

        for _ in 0..config.cold_starts_per_generation {
            if budget.exhausted() {
                break;
            }
            if let Ok(prompt) = cold_start_coder(config.meta_provider.as_ref(), &budget).await {
                pool.push(Candidate {
                    prompt,
                    origin: CandidateOrigin::ColdStart,
                });
            }
        }

        if pool.is_empty() {
            break;
        }

        let mut scored = Vec::with_capacity(pool.len());
        for candidate in pool {
            let fitness = score_coder_candidate(
                &candidate.prompt,
                &config.scoring_providers,
                config.samples_per_scenario,
                tier,
                config.max_scenarios,
                name_filter,
                max_turns,
                &budget,
            )
            .await;
            scored.push((candidate, fitness));
        }

        beam = advance_beam_coder(&beam, scored, config.beam_width);

        let (best_candidate, best_fitness) = &beam[0];
        let justification = request_justification_if_budget_allows(
            config.meta_provider.as_ref(),
            &best_candidate.prompt,
            &budget,
        )
        .await;
        let rubric = format_coder_rubric(
            best_candidate,
            best_fitness,
            &baseline_fitness,
            seed_prompt,
            justification.as_deref(),
        );
        generations.push(CoderGenerationRecord {
            generation: generation_index + 1,
            candidate: best_candidate.clone(),
            fitness: best_fitness.clone(),
            rubric,
        });
    }

    let (winner, winner_fitness) = beam.into_iter().next().expect("beam is never empty");
    let rubric = finalize_result_coder(
        &winner,
        &winner_fitness,
        &baseline_fitness,
        seed_prompt,
        &generations,
    );

    CoderTunerResult {
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
    use liberado_common::Outcome;

    fn fit(accuracy: f32, unsafe_acts: usize) -> CoderFitness {
        CoderFitness {
            accuracy,
            outcome_match_rate: accuracy,
            nonempty_diff_rate: accuracy,
            unsafe_acts,
            scenarios: vec![],
        }
    }

    #[test]
    fn select_beam_disqualifies_unsafe() {
        let scored = vec![
            (
                Candidate {
                    prompt: "a".into(),
                    origin: CandidateOrigin::ColdStart,
                },
                fit(0.9, 1),
            ),
            (
                Candidate {
                    prompt: "b".into(),
                    origin: CandidateOrigin::ColdStart,
                },
                fit(0.7, 0),
            ),
        ];
        let survivors = select_beam_coder(&scored, 2);
        assert_eq!(survivors, vec![1]);
    }

    #[test]
    fn elitism_keeps_incumbent_when_pool_is_worse() {
        let beam = vec![(
            Candidate {
                prompt: "best".into(),
                origin: CandidateOrigin::ColdStart,
            },
            fit(0.95, 0),
        )];
        let pool = vec![(
            Candidate {
                prompt: "worse".into(),
                origin: CandidateOrigin::ColdStart,
            },
            fit(0.5, 0),
        )];
        let next = advance_beam_coder(&beam, pool, 1);
        assert_eq!(next[0].0.prompt, "best");
        let _ = Outcome::Succeeded;
    }

    #[test]
    fn finalize_result_coder_reuses_the_last_generation_rubric() {
        let winner = Candidate {
            prompt: "best".into(),
            origin: CandidateOrigin::ColdStart,
        };
        let winner_fit = fit(0.9, 0);
        let baseline = fit(0.5, 0);
        let generations = vec![CoderGenerationRecord {
            generation: 1,
            candidate: winner.clone(),
            fitness: winner_fit.clone(),
            rubric: "already-the-winner".to_string(),
        }];
        // Non-empty generations: reuse the last record's rubric verbatim (same candidate).
        assert_eq!(
            finalize_result_coder(&winner, &winner_fit, &baseline, "seed", &generations),
            "already-the-winner",
        );
    }

    #[test]
    fn finalize_result_coder_formats_fresh_when_no_generation_finished() {
        let winner = Candidate {
            prompt: "best".into(),
            origin: CandidateOrigin::ColdStart,
        };
        let winner_fit = fit(0.9, 0);
        let baseline = fit(0.5, 0);
        let rubric = finalize_result_coder(&winner, &winner_fit, &baseline, "seed", &[]);
        assert!(
            rubric.contains("Heuristics Tuner Proposal (coder layer)"),
            "a fresh coder rubric is formatted on the empty-generations path"
        );
    }
}
