//! The generation loop: beam selection with Monte-Carlo-restart candidates, and the shared call
//! budget that bounds a tuning session (`docs/roadmap/heuristics-tuning-engine-plan.md`'s search
//! strategy). A human sets the budget per session — this module just enforces it.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use liberado_dispatcher::DEFAULT_SYSTEM_PROMPT;

use crate::candidate::{Candidate, CandidateOrigin};
use crate::config::TunerConfig;
use crate::generation::{cold_start, mutate, request_justification};
use crate::rubric::format_rubric;
use crate::scoring::{CandidateFitness, score_candidate};

/// A plain countdown shared across every concurrent LLM call in a session (scoring and meta
/// calls alike) — not a rate limiter or a fairness mechanism, just a cost ceiling. `Relaxed`
/// ordering is correct here: nothing else needs synchronizing against this counter, so the
/// stricter `AcqRel` a naive first pass might reach for buys nothing.
#[derive(Clone)]
pub struct Budget {
    remaining: Arc<AtomicUsize>,
}

impl Budget {
    pub fn new(calls: usize) -> Self {
        Self {
            remaining: Arc::new(AtomicUsize::new(calls)),
        }
    }

    /// Try to charge one call against the budget. Returns `true` if the call is authorized,
    /// `false` if the budget was already exhausted (in which case nothing is charged). Under
    /// concurrent access a handful of callers can briefly overshoot by one each — acceptable for
    /// a cost ceiling, not a hard resource lock.
    pub fn spend(&self) -> bool {
        // fetch_sub always decrements; if the pre-decrement value was 0, undo it and report
        // exhaustion rather than letting the counter go negative (it's a `usize`, so it would
        // wrap instead of going negative — restoring keeps the counter meaningful).
        let previous = self.remaining.fetch_sub(1, Ordering::Relaxed);
        if previous == 0 {
            self.remaining.fetch_add(1, Ordering::Relaxed);
            false
        } else {
            true
        }
    }

    pub fn exhausted(&self) -> bool {
        self.remaining.load(Ordering::Relaxed) == 0
    }
}

/// The best candidate found by the end of one generation, with its own rubric against the
/// baseline — not just the overall session's final winner. Saved as its own file so a human
/// reviewing the run later can see how the search actually got there, not just where it ended up.
pub struct GenerationRecord {
    /// 1-based, for human-facing filenames/output (`generation-1.txt`, ...).
    pub generation: usize,
    pub candidate: Candidate,
    pub fitness: CandidateFitness,
    pub rubric: String,
}

/// The result of a full tuning session: the winning candidate, its fitness, the baseline it beat
/// (or didn't), the formatted rubric ready to print/save, and every generation's own best
/// candidate + rubric along the way (`generations.last()` is the same candidate as `winner`).
pub struct TunerResult {
    pub winner: Candidate,
    pub winner_fitness: CandidateFitness,
    pub baseline_fitness: CandidateFitness,
    pub rubric: String,
    pub generations: Vec<GenerationRecord>,
}

/// Select the top `beam_width` candidates by fitness: `unsafe_acts > 0` disqualifies a candidate
/// outright (never selected, regardless of accuracy), then rank by accuracy descending, then
/// safe-default rate descending as a tiebreaker. Returns indices into `scored`, not clones, so the
/// caller decides what to do with the rest of the pool (the search loop just drops them).
///
/// Pure and directly unit-testable — the only judgment call this whole module makes about "which
/// candidates survive" lives here, not scattered through the async loop.
pub fn select_beam(scored: &[(Candidate, CandidateFitness)], beam_width: usize) -> Vec<usize> {
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
                fb.safe_default_rate
                    .partial_cmp(&fa.safe_default_rate)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });

    qualified.truncate(beam_width);
    qualified
}

/// Decide the next generation's beam from the current one plus a freshly-scored candidate pool —
/// **with elitism**: the incumbent beam is included in the same selection as the new pool, so a
/// generation that only produces candidates worse than the current beam can never regress it. A
/// real run surfaced why this matters: without it, `select_beam` only ever compared a generation's
/// new candidates against *each other*, so an independent cold start that happened to be merely
/// "safe" (0 unsafe acts) could permanently evict a much more accurate incumbent it was never
/// actually compared against, with no way back (`docs/roadmap/heuristics-tuning-engine-plan.md`'s
/// "comprehensive run" findings, 2026-07-06 — accuracy regressed 0.77 to 0.33 this way).
///
/// Falls back to the unchanged incumbent `beam` only if every candidate in the combined pool
/// (incumbents included) is disqualified — this can only happen if the incumbent beam itself still
/// carries an unsafe act (e.g. the seeded baseline, before it's ever been through this selection)
/// and nothing safe was found this generation either; a wasted generation, not a regression to
/// carry forward.
fn advance_beam(
    beam: &[(Candidate, CandidateFitness)],
    pool: Vec<(Candidate, CandidateFitness)>,
    beam_width: usize,
) -> Vec<(Candidate, CandidateFitness)> {
    let mut scored: Vec<(Candidate, CandidateFitness)> = beam.to_vec();
    scored.extend(pool);

    let survivors = select_beam(&scored, beam_width);
    if survivors.is_empty() {
        beam.to_vec()
    } else {
        survivors.into_iter().map(|idx| scored[idx].clone()).collect()
    }
}

/// Run a full tuning session: score the baseline, then cycle generations of mutations + cold
/// starts, keeping a beam of the best-so-far, until either `max_generations` or the call budget is
/// exhausted. Never touches a real tool or vault — every call in this module is either a
/// classification-only `Dispatcher::dispatch` or a meta-generation call proposing prompt text.
pub async fn run_tuner(config: TunerConfig) -> TunerResult {
    let budget = Budget::new(config.call_budget);

    let baseline_fitness = score_candidate(
        DEFAULT_SYSTEM_PROMPT,
        &config.scoring_providers,
        config.samples_per_scenario,
        config.max_scenarios,
        &budget,
    )
    .await;
    let baseline = Candidate {
        prompt: DEFAULT_SYSTEM_PROMPT.to_string(),
        origin: CandidateOrigin::ColdStart,
    };

    let mut beam: Vec<(Candidate, CandidateFitness)> = vec![(baseline, baseline_fitness.clone())];
    let mut generations: Vec<GenerationRecord> = Vec::new();

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
                match mutate(config.meta_provider.as_ref(), &parent.prompt, &failing, &budget).await {
                    Ok(prompt) => pool.push(Candidate {
                        prompt,
                        origin: CandidateOrigin::MutatedFrom {
                            parent_index,
                            parent_accuracy: parent_fitness.accuracy,
                        },
                    }),
                    Err(_) => continue, // logged inside mutate(); skip this slot, not the run
                }
            }
        }

        for _ in 0..config.cold_starts_per_generation {
            if budget.exhausted() {
                break;
            }
            if let Ok(prompt) = cold_start(config.meta_provider.as_ref(), &budget).await {
                pool.push(Candidate {
                    prompt,
                    origin: CandidateOrigin::ColdStart,
                });
            }
        }

        if pool.is_empty() {
            break; // budget ran out before a single candidate could be produced this generation
        }

        let mut scored = Vec::with_capacity(pool.len());
        for candidate in pool {
            let fitness = score_candidate(
                &candidate.prompt,
                &config.scoring_providers,
                config.samples_per_scenario,
                config.max_scenarios,
                &budget,
            )
            .await;
            scored.push((candidate, fitness));
        }

        beam = advance_beam(&beam, scored, config.beam_width);

        // Record this generation's best-so-far, with its own justification call — a human
        // reviewing later gets the search's progression, not just where it ended up.
        let (best_candidate, best_fitness) = &beam[0];
        let justification = request_justification_if_budget_allows(
            config.meta_provider.as_ref(),
            &best_candidate.prompt,
            &budget,
        )
        .await;
        let rubric = format_rubric(
            best_candidate,
            best_fitness,
            &baseline_fitness,
            justification.as_deref(),
        );
        generations.push(GenerationRecord {
            generation: generation_index + 1,
            candidate: best_candidate.clone(),
            fitness: best_fitness.clone(),
            rubric,
        });
    }

    let (winner, winner_fitness) = beam.into_iter().next().expect("beam is never empty");
    // The final generation's record already carries the same candidate + a rubric against the
    // baseline — reuse it rather than spending another justification call on an identical prompt.
    let rubric = generations
        .last()
        .map(|g| g.rubric.clone())
        .unwrap_or_else(|| format_rubric(&winner, &winner_fitness, &baseline_fitness, None));

    TunerResult {
        winner,
        winner_fitness,
        baseline_fitness,
        rubric,
        generations,
    }
}

/// Request a justification unless the budget is already exhausted — a best-effort call, not one
/// that should ever abort a run. Shared by every generation's record and the final result.
async fn request_justification_if_budget_allows(
    meta_provider: &dyn liberado_provider::Provider,
    prompt: &str,
    budget: &Budget,
) -> Option<String> {
    if budget.exhausted() {
        return None;
    }
    request_justification(meta_provider, prompt, budget).await.ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::candidate::CandidateOrigin;

    fn fitness(accuracy: f32, safe_default_rate: f32, unsafe_acts: usize) -> CandidateFitness {
        CandidateFitness {
            accuracy,
            safe_default_rate,
            unsafe_acts,
            scenarios: Vec::new(),
        }
    }

    fn candidate(prompt: &str) -> Candidate {
        Candidate {
            prompt: prompt.to_string(),
            origin: CandidateOrigin::ColdStart,
        }
    }

    #[test]
    fn budget_spends_down_to_zero_then_refuses() {
        let budget = Budget::new(2);
        assert!(budget.spend());
        assert!(budget.spend());
        assert!(!budget.spend());
        assert!(budget.exhausted());
    }

    #[test]
    fn budget_restore_on_exhausted_spend_does_not_go_negative() {
        let budget = Budget::new(0);
        assert!(!budget.spend());
        // A second call must still cleanly refuse, not wrap around from an unrestored decrement.
        assert!(!budget.spend());
    }

    #[test]
    fn select_beam_excludes_unsafe_candidates_even_at_top_accuracy() {
        let scored = vec![
            (candidate("unsafe-but-accurate"), fitness(0.95, 1.0, 1)),
            (candidate("safe-but-less-accurate"), fitness(0.80, 1.0, 0)),
        ];
        let beam = select_beam(&scored, 2);
        assert_eq!(beam, vec![1]);
    }

    #[test]
    fn select_beam_orders_by_accuracy_then_safe_default_rate() {
        let scored = vec![
            (candidate("a"), fitness(0.80, 0.5, 0)),
            (candidate("b"), fitness(0.90, 1.0, 0)),
            (candidate("c"), fitness(0.90, 0.5, 0)),
        ];
        let beam = select_beam(&scored, 3);
        // b and c tie on accuracy; b wins the tiebreak on safe_default_rate.
        assert_eq!(beam, vec![1, 2, 0]);
    }

    #[test]
    fn select_beam_truncates_to_width() {
        let scored = vec![
            (candidate("a"), fitness(0.9, 1.0, 0)),
            (candidate("b"), fitness(0.8, 1.0, 0)),
            (candidate("c"), fitness(0.7, 1.0, 0)),
        ];
        assert_eq!(select_beam(&scored, 1), vec![0]);
    }

    #[test]
    fn select_beam_all_disqualified_returns_empty_not_a_panic() {
        let scored = vec![
            (candidate("a"), fitness(0.9, 1.0, 1)),
            (candidate("b"), fitness(0.8, 1.0, 2)),
        ];
        assert!(select_beam(&scored, 2).is_empty());
    }

    #[test]
    fn select_beam_handles_fewer_candidates_than_width() {
        let scored = vec![(candidate("a"), fitness(0.9, 1.0, 0))];
        assert_eq!(select_beam(&scored, 5), vec![0]);
    }

    #[test]
    fn advance_beam_never_regresses_below_a_safe_incumbent() {
        // The exact real-world case that motivated this function: an incumbent that's both safe
        // and accurate must survive a generation whose only new candidate is safe but much worse.
        let beam = vec![(candidate("incumbent"), fitness(0.77, 1.0, 0))];
        let pool = vec![(candidate("regressive-cold-start"), fitness(0.33, 1.0, 0))];
        let next = advance_beam(&beam, pool, 1);
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].0.prompt, "incumbent");
        assert_eq!(next[0].1.accuracy, 0.77);
    }

    #[test]
    fn advance_beam_adopts_a_genuinely_better_new_candidate() {
        let beam = vec![(candidate("incumbent"), fitness(0.77, 1.0, 0))];
        let pool = vec![(candidate("improved-mutation"), fitness(0.90, 1.0, 0))];
        let next = advance_beam(&beam, pool, 1);
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].0.prompt, "improved-mutation");
        assert_eq!(next[0].1.accuracy, 0.90);
    }

    #[test]
    fn advance_beam_replaces_an_unsafe_incumbent_with_a_safe_candidate_even_at_lower_accuracy() {
        let beam = vec![(candidate("unsafe-baseline"), fitness(0.95, 1.0, 1))];
        let pool = vec![(candidate("safe-candidate"), fitness(0.50, 1.0, 0))];
        let next = advance_beam(&beam, pool, 1);
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].0.prompt, "safe-candidate");
    }

    #[test]
    fn advance_beam_falls_back_to_incumbent_when_everything_is_disqualified() {
        let beam = vec![(candidate("unsafe-incumbent"), fitness(0.9, 1.0, 1))];
        let pool = vec![(candidate("also-unsafe"), fitness(0.5, 1.0, 2))];
        let next = advance_beam(&beam, pool, 1);
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].0.prompt, "unsafe-incumbent");
    }

    #[tokio::test]
    #[ignore = "requires OPENROUTER_API_KEY + network access"]
    async fn live_end_to_end() {
        // Deliberately tiny: one generation, one mutation, one cold start -- enough to smoke the
        // full pipeline (scoring, generation, beam selection, rubric) cheaply. Override
        // TUNER_MAX_GENERATIONS / TUNER_CALL_BUDGET via env if a longer run is wanted.
        // SAFETY: single-threaded test process assumption -- these vars aren't read by any other
        // concurrently-running test, only by TunerConfig::load() called immediately after.
        unsafe {
            std::env::set_var("TUNER_MAX_GENERATIONS", "1");
            std::env::set_var("TUNER_MUTATIONS_PER_CANDIDATE", "1");
            std::env::set_var("TUNER_COLD_STARTS_PER_GENERATION", "1");
            std::env::set_var("TUNER_BEAM_WIDTH", "1");
            std::env::set_var("TUNER_CALL_BUDGET", "60");
        }
        let config = TunerConfig::load().expect("OPENROUTER_API_KEY not set");
        let result = run_tuner(config).await;
        assert!(!result.rubric.is_empty());
        assert_eq!(
            result.winner_fitness.unsafe_acts, 0,
            "a winner must never carry an unsafe act"
        );
        assert!(
            !result.generations.is_empty(),
            "at least one generation record should exist for review"
        );
        assert_eq!(result.generations.last().unwrap().rubric, result.rubric);
    }
}
