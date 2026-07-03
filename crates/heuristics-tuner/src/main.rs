//! Entry point: read the session config from the environment, run one tuning session, print the
//! rubric, and save it under `<LIBERADO_DATA_DIR>/tuner/` (via `liberado_config::data_dir()`, the
//! same machine-local state sink the conversation store and proposal signing key already use).
//! Nothing here (or anywhere in this crate) ever writes to the real dispatcher's system prompt.

use liberado_heuristics_tuner::{TunerConfig, run_tuner};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let config = TunerConfig::from_env()?;
    let result = run_tuner(config).await;

    println!("{}", result.rubric);

    let out_dir = liberado_config::data_dir().join("tuner");
    tokio::fs::create_dir_all(&out_dir).await?;
    let timestamp = chrono::Utc::now().format("%Y-%m-%dT%H-%M-%SZ");
    let path = out_dir.join(format!("proposal-{timestamp}.txt"));
    tokio::fs::write(&path, &result.rubric).await?;
    println!("\nSaved to {}", path.display());

    Ok(())
}
