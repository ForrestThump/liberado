//! Minimal process boundary for the Rust-native coding backend.
//!
//! This binary intentionally speaks the stable `liberado-coder-core` JSON contracts so callers
//! such as `liberado-pr-dispatch-mcp` can replace a subprocess backend without linking directly
//! against the full in-process loop stack.

use std::{
    env,
    path::{Path, PathBuf},
    sync::Arc,
};

use liberado_coder_agent::LiberadoLoopBackend;
use liberado_coder_core::{CoderBackend, CoderRunRequest};
use liberado_config_loader::{ProviderProfile, Topology};
use liberado_provider::Provider;
use liberado_provider_openai_compat::OpenAiCompatibleProvider;

const PROVIDER_ENV: &str = "LIBERADO_CODER_PROVIDER";

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "liberado_coder_runner=info".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    if let Err(error) = run().await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let args = Args::parse(env::args().skip(1))?;
    let request = read_request(args.request.as_deref()).await?;
    let profile = provider_profile(args.config_dir.as_deref())?;
    let provider = provider_from_profile(&profile, &request.config.coder.model)?;
    let backend = LiberadoLoopBackend::new(provider);
    let result = backend
        .run(request)
        .await
        .map_err(|error| format!("coder backend failed: {error}"))?;
    let json = serde_json::to_string_pretty(&result)
        .map_err(|error| format!("serialize coder result: {error}"))?;
    println!("{json}");
    Ok(())
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Args {
    request: Option<PathBuf>,
    config_dir: Option<PathBuf>,
}

impl Args {
    fn parse(mut args: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut parsed = Args::default();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--request" => {
                    parsed.request =
                        Some(PathBuf::from(args.next().ok_or_else(|| {
                            "--request requires a path or '-'".to_string()
                        })?));
                }
                "--config-dir" => {
                    parsed.config_dir = Some(PathBuf::from(
                        args.next()
                            .ok_or_else(|| "--config-dir requires a path".to_string())?,
                    ));
                }
                "--help" | "-h" => return Err(usage()),
                other => return Err(format!("unknown argument '{other}'\n{}", usage())),
            }
        }
        Ok(parsed)
    }
}

async fn read_request(path: Option<&Path>) -> Result<CoderRunRequest, String> {
    let bytes = match path {
        Some(path) if path.as_os_str() != "-" => tokio::fs::read(path)
            .await
            .map_err(|error| format!("read request {}: {error}", path.display()))?,
        _ => {
            use tokio::io::AsyncReadExt;
            let mut bytes = Vec::new();
            tokio::io::stdin()
                .read_to_end(&mut bytes)
                .await
                .map_err(|error| format!("read request from stdin: {error}"))?;
            bytes
        }
    };
    serde_json::from_slice(&bytes).map_err(|error| format!("parse CoderRunRequest JSON: {error}"))
}

fn provider_profile(config_dir: Option<&Path>) -> Result<ProviderProfile, String> {
    let topology = match config_dir {
        Some(dir) => read_topology(dir)?,
        None => Topology::default(),
    };
    let provider_name = env::var(PROVIDER_ENV).unwrap_or_else(|_| topology.provider.clone());
    topology
        .providers
        .into_iter()
        .find(|profile| profile.name == provider_name)
        .ok_or_else(|| format!("provider '{provider_name}' is not declared in topology.providers"))
}

fn read_topology(config_dir: &Path) -> Result<Topology, String> {
    let path = config_dir.join("topology.toml");
    if !path.exists() {
        return Ok(Topology::default());
    }
    let raw = std::fs::read_to_string(&path)
        .map_err(|error| format!("read topology {}: {error}", path.display()))?;
    toml::from_str(&raw).map_err(|error| format!("parse topology {}: {error}", path.display()))
}

fn provider_from_profile(
    profile: &ProviderProfile,
    model: &str,
) -> Result<Arc<dyn Provider>, String> {
    let api_key = env::var(&profile.api_key_env).map_err(|_| {
        format!(
            "{} is required for provider '{}'",
            profile.api_key_env, profile.name
        )
    })?;
    let provider = OpenAiCompatibleProvider::new(api_key, model, &profile.base_url)
        .with_extra_client_error_status(profile.extra_client_error_status.clone());
    Ok(Arc::new(provider))
}

fn usage() -> String {
    "usage: liberado-coder-run [--request <path|->] [--config-dir <dir>]".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_args() {
        let args = Args::parse(
            ["--request", "request.json", "--config-dir", "config"]
                .into_iter()
                .map(str::to_string),
        )
        .unwrap();
        assert_eq!(
            args,
            Args {
                request: Some(PathBuf::from("request.json")),
                config_dir: Some(PathBuf::from("config")),
            }
        );
    }

    #[test]
    fn unknown_arg_is_an_error() {
        let err = Args::parse(["--wat"].into_iter().map(str::to_string)).unwrap_err();
        assert!(err.contains("unknown argument"));
    }
}
