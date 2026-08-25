//! Argument parsing for the `liberado-worker` binary.
//!
//! Lives in the library on purpose: coverage gates attribute per-function results to
//! library targets only, so a parser buried in a bin scores as permanently untested no
//! matter what its tests do (the CRAP ratchet reads that as cc²+cc). Both argv and the
//! environment arrive as parameters, so tests never touch process-global state.

use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq)]
pub struct Args {
    pub bind: String,
    pub data_dir: PathBuf,
    pub config_dir: Option<PathBuf>,
    pub model: Option<String>,
    pub forge_url: Option<String>,
    pub forge_token: String,
    /// Base for resolving relative repository paths to clone URLs; defaults to the
    /// forge itself when unset.
    pub clone_base_url: Option<String>,
    pub token: String,
    pub max_concurrent: usize,
}

/// The type of [`std::env::args`] and [`std::env::var`], narrowed to what parsing needs.
pub trait EnvLookup {
    fn var(&self, name: &str) -> Option<String>;
}

/// Reads the real process environment.
pub struct ProcessEnv;

impl EnvLookup for ProcessEnv {
    fn var(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }
}

impl Args {
    /// Model precedence: the explicit flag wins, otherwise the provider profile's
    /// declared default — topology alone must be able to configure a worker end to end,
    /// and a profile's endpoint, key, and default model belong together.
    pub fn effective_model(&self, profile_default_model: &str) -> String {
        self.model
            .clone()
            .unwrap_or_else(|| profile_default_model.to_string())
    }
}

pub fn parse_args(
    argv: impl Iterator<Item = String>,
    env: &impl EnvLookup,
) -> Result<Args, String> {
    let mut args = Args {
        bind: env
            .var("LIBERADO_WORKER_BIND")
            .unwrap_or_else(|| "127.0.0.1:7780".into()),
        data_dir: PathBuf::from(
            env.var("LIBERADO_DATA_DIR")
                .unwrap_or_else(|| ".liberado".into()),
        ),
        config_dir: env.var("LIBERADO_CONFIG_DIR").map(PathBuf::from),
        model: None,
        forge_url: env.var("LIBERADO_FORGE_URL"),
        clone_base_url: None,
        forge_token: String::new(),
        token: String::new(),
        max_concurrent: 2,
    };
    let mut iter = argv;
    while let Some(flag) = iter.next() {
        apply_flag(&mut args, &flag, &mut iter, env)?;
    }
    if args.token.is_empty() {
        args.token = env
            .var("LIBERADO_WORKER_TOKEN")
            .ok_or_else(|| usage("LIBERADO_WORKER_TOKEN must be set (or pass --token-env)"))?;
    }
    if args.forge_token.is_empty() {
        args.forge_token = env.var("LIBERADO_FORGE_TOKEN").unwrap_or_default();
    }
    Ok(args)
}

/// Apply one `--flag [value]` pair. Split from [`parse_args`] so adding a flag costs
/// the loop no complexity — the CRAP ratchet reads this file.
fn apply_flag(
    args: &mut Args,
    flag: &str,
    iter: &mut impl Iterator<Item = String>,
    env: &impl EnvLookup,
) -> Result<(), String> {
    let mut value = |name: &str| iter.next().ok_or_else(|| format!("{name} needs a value"));
    match flag {
        "--bind" => args.bind = value("--bind")?,
        "--data-dir" => args.data_dir = PathBuf::from(value("--data-dir")?),
        "--config-dir" => args.config_dir = Some(PathBuf::from(value("--config-dir")?)),
        "--model" => args.model = Some(value("--model")?),
        "--forge-url" => args.forge_url = Some(value("--forge-url")?),
        "--clone-base-url" => args.clone_base_url = Some(value("--clone-base-url")?),
        "--forge-token-env" => {
            let name = value("--forge-token-env")?;
            args.forge_token = env.var(&name).ok_or_else(|| format!("{name} is not set"))?;
        }
        "--token-env" => {
            let name = value("--token-env")?;
            args.token = env.var(&name).ok_or_else(|| format!("{name} is not set"))?;
        }
        "--max-concurrent" => {
            args.max_concurrent = value("--max-concurrent")?
                .parse()
                .map_err(|_| "--max-concurrent wants a number")?
        }
        other => return Err(usage(format!("unknown argument: {other}"))),
    }
    Ok(())
}

fn usage(message: impl std::fmt::Display) -> String {
    format!(
        "{message}\n\nusage: liberado-worker [--bind ADDR] [--data-dir PATH] [--config-dir PATH]\n\
         \x20                [--model NAME] [--forge-url URL] [--forge-token-env VAR]\n\
         \x20                [--clone-base-url URL] [--token-env VAR] [--max-concurrent N]\n\n\
         Env: LIBERADO_WORKER_BIND LIBERADO_WORKER_TOKEN LIBERADO_DATA_DIR\n\
         \x20\x20\x20\x20 LIBERADO_CONFIG_DIR LIBERADO_CODER_PROVIDER LIBERADO_FORGE_URL LIBERADO_FORGE_TOKEN"
    )
}

#[cfg(test)]
mod tests;
