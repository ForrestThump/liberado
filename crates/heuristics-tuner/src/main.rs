//! Entry point: resolve the session config from `tuner.toml` + environment overrides
//! (`config::TunerConfig::load`), run one tuning session for whichever role `config.layer`
//! selects, and save every generation's best candidate — not just the final winner — as its own
//! file under `<LIBERADO_DATA_DIR>/tuner/<run-timestamp>/`, via `liberado_config::data_dir()` (the
//! same machine-local state sink the conversation store and proposal signing key already use).
//! Nothing here (or anywhere in this crate) ever writes to a real prompt const.

use liberado_heuristics_tuner::{
    CoderTunerResult, DEFAULT_CODER_PROMPT_PATH, DEFAULT_CODER_SYSTEM_PROMPT, ExecutorTunerResult,
    Layer, TunerConfig, TunerResult, build_coder_draft_proposal, run_coder_tuner,
    run_executor_tuner, run_subagent_tuner, run_tuner, write_coder_draft_proposal,
};
use std::path::Path;

/// Shared by the executor and subagent branches below — both produce an `ExecutorTunerResult` via
/// the same underlying search loop (`tool_loop_search::run_tool_loop_tuner`), differing only in
/// which prompt const seeded it.
async fn save_tool_loop_result(
    result: ExecutorTunerResult,
    out_dir: &Path,
) -> Result<String, Box<dyn std::error::Error>> {
    for record in &result.generations {
        let path = out_dir.join(format!("generation-{}.txt", record.generation));
        tokio::fs::write(&path, &record.rubric).await?;
        println!(
            "Generation {} — accuracy {:.2}, outcome-match {:.2}, unsafe acts {} -> {}",
            record.generation,
            record.fitness.accuracy,
            record.fitness.outcome_match_rate,
            record.fitness.unsafe_acts,
            path.display()
        );
    }
    Ok(result.rubric)
}

async fn save_coder_result(
    result: CoderTunerResult,
    out_dir: &Path,
) -> Result<String, Box<dyn std::error::Error>> {
    for record in &result.generations {
        let path = out_dir.join(format!("generation-{}.txt", record.generation));
        tokio::fs::write(&path, &record.rubric).await?;
        println!(
            "Generation {} — coding accuracy {:.2}, nonempty-diff {:.2}, unsafe {} -> {}",
            record.generation,
            record.fitness.accuracy,
            record.fitness.nonempty_diff_rate,
            record.fitness.unsafe_acts,
            path.display()
        );
    }

    // Meta-loop seed (Decision 14): draft proposal artifacts only — never write prompts/ live.
    let proposal = build_coder_draft_proposal(
        &result.winner,
        &result.winner_fitness,
        &result.baseline_fitness,
        DEFAULT_CODER_SYSTEM_PROMPT,
        DEFAULT_CODER_PROMPT_PATH,
    );
    let written = write_coder_draft_proposal(out_dir, &proposal).await?;
    println!(
        "Draft proposal recommended={} reason={} files={}",
        proposal.recommended,
        proposal.reason,
        written.len()
    );
    for path in &written {
        println!("  proposal artifact: {}", path.display());
    }

    Ok(result.rubric)
}

/// Save a dispatcher tuner session's per-generation records, mirroring `save_tool_loop_result` /
/// `save_coder_result` below. Lives here (not in `search.rs`) because only the binary persists —
/// the library returns the result for a caller to render as it likes.
async fn save_dispatcher_result(
    result: TunerResult,
    out_dir: &Path,
) -> Result<String, Box<dyn std::error::Error>> {
    for record in &result.generations {
        let path = out_dir.join(format!("generation-{}.txt", record.generation));
        tokio::fs::write(&path, &record.rubric).await?;
        println!(
            "Generation {} — accuracy {:.2}, safe-default {:.2}, unsafe acts {} -> {}",
            record.generation,
            record.fitness.accuracy,
            record.fitness.safe_default_rate,
            record.fitness.unsafe_acts,
            path.display()
        );
    }
    Ok(result.rubric)
}

/// Resolve the run's output directory and create it (and any parents) before any tuner work
/// writes there. `data_dir()` is the same resolver the daemon uses, so results land where
/// operators already look.
async fn prepare_run_dir() -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    let run_timestamp = chrono::Utc::now().format("%Y-%m-%dT%H-%M-%SZ").to_string();
    let out_dir = liberado_config::data_dir()
        .join("tuner")
        .join(&run_timestamp);
    tokio::fs::create_dir_all(&out_dir).await?;
    Ok(out_dir)
}

/// Run whichever layer `config.layer` selects and persist its per-generation artifacts.
///
/// Extracted verbatim from `main` so the driver stays thin: this is the one place that knows
/// which search loop and which saver belong to each layer, and it only runs against a live
/// provider (see the module docs), so it is deliberately untested — everything below it that
/// touches files instead of models is.
async fn run_selected_layer(
    config: TunerConfig,
    out_dir: &Path,
) -> Result<String, Box<dyn std::error::Error>> {
    match config.layer {
        Layer::Coder => save_coder_result(run_coder_tuner(config).await, out_dir).await,
        layer => run_tool_loop_layer(layer, config, out_dir).await,
    }
}

/// The dispatcher, executor, and subagent layers all end in an `ExecutorTunerResult`-shaped
/// save; only the search loop that produces it differs (see the module docs).
async fn run_tool_loop_layer(
    layer: Layer,
    config: TunerConfig,
    out_dir: &Path,
) -> Result<String, Box<dyn std::error::Error>> {
    match layer {
        Layer::Dispatcher => save_dispatcher_result(run_tuner(config).await, out_dir).await,
        Layer::Executor => save_tool_loop_result(run_executor_tuner(config).await, out_dir).await,
        Layer::Subagent => save_tool_loop_result(run_subagent_tuner(config).await, out_dir).await,
        // run_selected_layer routes Coder before this function; an explicit
        // failing arm keeps a future Layer variant from silently tuning the
        // wrong layer.
        Layer::Coder => Err(
            "the coder layer is routed by run_selected_layer and cannot reach the tool-loop path"
                .into(),
        ),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let config = TunerConfig::load()?;
    let out_dir = prepare_run_dir().await?;
    let final_rubric = run_selected_layer(config, &out_dir).await?;
    write_final_result(&final_rubric, &out_dir).await
}

/// Write `final.txt` — the operator-facing result, always at the same name under the run dir —
/// and print the path plus the rubric to stdout.
async fn write_final_result(
    final_rubric: &str,
    out_dir: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let final_path = out_dir.join("final.txt");
    tokio::fs::write(&final_path, final_rubric).await?;

    println!(
        "
=== Final result: {} ===
",
        final_path.display()
    );
    println!("{final_rubric}");
    println!(
        "
All files saved under {}",
        out_dir.display()
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_heuristics_tuner::{
        Candidate, CandidateFitness, CandidateOrigin, CoderFitness, CoderGenerationRecord,
        CoderTunerResult, ExecutorGenerationRecord, ExecutorTunerResult, GenerationRecord,
        ToolLoopFitness, TunerResult,
    };

    fn origin() -> CandidateOrigin {
        CandidateOrigin::ColdStart
    }

    fn candidate(text: &str) -> Candidate {
        Candidate {
            prompt: text.into(),
            origin: origin(),
        }
    }

    fn dispatcher_fitness(accuracy: f32) -> CandidateFitness {
        CandidateFitness {
            accuracy,
            safe_default_rate: 1.0,
            unsafe_acts: 0,
            scenarios: Vec::new(),
        }
    }

    fn tool_loop_fitness(accuracy: f32) -> ToolLoopFitness {
        ToolLoopFitness {
            accuracy,
            outcome_match_rate: accuracy,
            unsafe_acts: 0,
            scenarios: Vec::new(),
        }
    }

    fn coder_fitness(accuracy: f32) -> CoderFitness {
        CoderFitness {
            accuracy,
            outcome_match_rate: 1.0,
            nonempty_diff_rate: 1.0,
            unsafe_acts: 0,
            scenarios: Vec::new(),
        }
    }

    #[tokio::test]
    async fn dispatcher_result_writes_one_file_per_generation_plus_final() {
        let result = TunerResult {
            winner: candidate("w"),
            winner_fitness: dispatcher_fitness(0.9),
            baseline_fitness: dispatcher_fitness(0.5),
            rubric: "final rubric".into(),
            generations: vec![GenerationRecord {
                generation: 1,
                candidate: candidate("g1"),
                fitness: dispatcher_fitness(0.7),
                rubric: "gen 1 rubric".into(),
            }],
        };
        let out = tempfile::tempdir().unwrap();
        let saved = save_dispatcher_result(result, out.path()).await.unwrap();
        assert_eq!(saved, "final rubric");
        assert_eq!(
            std::fs::read_to_string(out.path().join("generation-1.txt")).unwrap(),
            "gen 1 rubric"
        );
    }

    #[tokio::test]
    async fn tool_loop_result_writes_every_generation_file() {
        let result = ExecutorTunerResult {
            winner: candidate("w"),
            winner_fitness: tool_loop_fitness(0.9),
            baseline_fitness: tool_loop_fitness(0.4),
            rubric: "tool loop final".into(),
            generations: vec![
                ExecutorGenerationRecord {
                    generation: 1,
                    candidate: candidate("g1"),
                    fitness: tool_loop_fitness(0.6),
                    rubric: "gen 1".into(),
                },
                ExecutorGenerationRecord {
                    generation: 2,
                    candidate: candidate("g2"),
                    fitness: tool_loop_fitness(0.8),
                    rubric: "gen 2".into(),
                },
            ],
        };
        let out = tempfile::tempdir().unwrap();
        let saved = save_tool_loop_result(result, out.path()).await.unwrap();
        assert_eq!(saved, "tool loop final");
        for generation in 1..=2 {
            let path = out.path().join(format!("generation-{generation}.txt"));
            assert!(path.exists(), "{} missing", path.display());
        }
    }

    #[tokio::test]
    async fn coder_result_also_leaves_draft_proposal_artifacts() {
        let result = CoderTunerResult {
            winner: candidate("improved prompt"),
            winner_fitness: coder_fitness(0.9),
            baseline_fitness: coder_fitness(0.5),
            rubric: "coder final".into(),
            generations: vec![CoderGenerationRecord {
                generation: 1,
                candidate: candidate("g1"),
                fitness: coder_fitness(0.7),
                rubric: "gen 1".into(),
            }],
        };
        let out = tempfile::tempdir().unwrap();
        let saved = save_coder_result(result, out.path()).await.unwrap();
        assert_eq!(saved, "coder final");
        assert!(out.path().join("generation-1.txt").exists());
        // The draft-proposal artifacts land beside the generations (never into prompts/).
        let artifacts: Vec<_> = std::fs::read_dir(out.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            artifacts.iter().any(|name| name.starts_with("proposal")),
            "expected a proposal artifact among {artifacts:?}"
        );
    }

    #[tokio::test]
    async fn write_final_result_names_the_file_and_saves_the_rubric() {
        let out = tempfile::tempdir().unwrap();
        write_final_result("the answer", out.path()).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(out.path().join("final.txt")).unwrap(),
            "the answer"
        );
    }
}
