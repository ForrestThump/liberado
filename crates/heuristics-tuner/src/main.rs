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

/// Resolve the run's output directory from the loaded config, creating it (and any parents) up
/// front. Kept out of `main` so the binary's entry is a flat sequence of two calls and the setup
/// (which carries a config load + fs create, each a `?` branch) is separately understandable.
async fn prepare_run_dir() -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    let run_timestamp = chrono::Utc::now().format("%Y-%m-%dT%H-%M-%SZ").to_string();
    let out_dir = liberado_config::data_dir()
        .join("tuner")
        .join(&run_timestamp);
    tokio::fs::create_dir_all(&out_dir).await?;
    Ok(out_dir)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let config = TunerConfig::load()?;
    let layer = config.layer;
    let out_dir = prepare_run_dir().await?;

    let final_rubric = match layer {
        Layer::Dispatcher => save_dispatcher_result(run_tuner(config).await, &out_dir).await?,
        Layer::Executor => {
            save_tool_loop_result(run_executor_tuner(config).await, &out_dir).await?
        }
        Layer::Subagent => {
            save_tool_loop_result(run_subagent_tuner(config).await, &out_dir).await?
        }
        Layer::Coder => save_coder_result(run_coder_tuner(config).await, &out_dir).await?,
    };

    write_final_result(&final_rubric, &out_dir).await
}

/// Write the session's final answer under a stable filename and print the result summary - the
/// tail `main` used to inline, whose two `?` (write) and three prints carried decision weight.
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
