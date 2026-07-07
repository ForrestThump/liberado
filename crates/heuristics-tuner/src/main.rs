//! Entry point: resolve the session config from `tuner.toml` + environment overrides
//! (`config::TunerConfig::load`), run one tuning session for whichever role `config.layer`
//! selects, and save every generation's best candidate — not just the final winner — as its own
//! file under `<LIBERADO_DATA_DIR>/tuner/<run-timestamp>/`, via `liberado_config::data_dir()` (the
//! same machine-local state sink the conversation store and proposal signing key already use).
//! Nothing here (or anywhere in this crate) ever writes to a real prompt const.

use liberado_heuristics_tuner::{ExecutorTunerResult, Layer, TunerConfig, run_executor_tuner, run_subagent_tuner, run_tuner};
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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let config = TunerConfig::load()?;
    let layer = config.layer;

    let run_timestamp = chrono::Utc::now().format("%Y-%m-%dT%H-%M-%SZ").to_string();
    let out_dir = liberado_config::data_dir().join("tuner").join(&run_timestamp);
    tokio::fs::create_dir_all(&out_dir).await?;

    let final_rubric = match layer {
        Layer::Dispatcher => {
            let result = run_tuner(config).await;
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
            result.rubric
        }
        Layer::Executor => save_tool_loop_result(run_executor_tuner(config).await, &out_dir).await?,
        Layer::Subagent => save_tool_loop_result(run_subagent_tuner(config).await, &out_dir).await?,
    };

    // A stable, obvious filename for "the answer" — the same content as the last generation's
    // file, so a human doesn't have to know how many generations ran to find the final result.
    let final_path = out_dir.join("final.txt");
    tokio::fs::write(&final_path, &final_rubric).await?;

    println!("\n=== Final result: {} ===\n", final_path.display());
    println!("{}", final_rubric);
    println!("\nAll files saved under {}", out_dir.display());

    Ok(())
}
