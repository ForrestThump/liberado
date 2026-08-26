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
    /// Accept self-signed / private-CA certificates on the forge URL. Deliberate
    /// opt-in for LAN forges; never a default.
    pub forge_insecure_tls: bool,
    /// Base for resolving relative repository paths to clone URLs; defaults to the
    /// forge itself when unset.
    pub clone_base_url: Option<String>,
    pub token: String,
    pub max_concurrent: usize,
    pub question_timeout_secs: u64,
    pub max_open_questions: u32,
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
        forge_insecure_tls: matches!(
            env.var("LIBERADO_FORGE_INSECURE_TLS").as_deref(),
            Some("1") | Some("true")
        ),
        token: String::new(),
        max_concurrent: 2,
        question_timeout_secs: env
            .var("LIBERADO_WORKER_QUESTION_TIMEOUT_SECS")
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(900),
        max_open_questions: env
            .var("LIBERADO_WORKER_MAX_OPEN_QUESTIONS")
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(3),
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
    match flag {
        "--bind" | "--data-dir" | "--config-dir" | "--model" | "--forge-url"
        | "--clone-base-url" => apply_value_flag(args, flag, iter),
        "--forge-token-env" | "--token-env" => apply_token_env_flag(args, flag, iter, env),
        "--forge-insecure-tls" => {
            args.forge_insecure_tls = true;
            Ok(())
        }
        "--max-concurrent" | "--question-timeout-secs" | "--max-open-questions" => {
            apply_numeric_flag(args, flag, iter)
        }
        other => Err(usage(format!("unknown argument: {other}"))),
    }
}

/// Numeric flags share one arm so adding another costs the flag loop nothing; this
/// helper owns their assignment.
fn apply_numeric_flag(
    args: &mut Args,
    flag: &str,
    iter: &mut impl Iterator<Item = String>,
) -> Result<(), String> {
    match flag {
        "--max-concurrent" => args.max_concurrent = parse_number(iter, flag)?,
        "--question-timeout-secs" => args.question_timeout_secs = parse_number(iter, flag)?,
        other => args.max_open_questions = parse_number::<u32>(iter, other)?,
    }
    Ok(())
}

/// Parse any numeric flag value with the flag's name in the error.
fn parse_number<T: std::str::FromStr>(
    iter: &mut impl Iterator<Item = String>,
    flag: &str,
) -> Result<T, String> {
    let raw = iter.next().ok_or_else(|| format!("{flag} needs a value"))?;
    raw.parse().map_err(|_| format!("{flag} wants a number"))
}

/// Flags whose value lands in one string-ish field, named after the flag itself.
fn apply_value_flag(
    args: &mut Args,
    flag: &str,
    iter: &mut impl Iterator<Item = String>,
) -> Result<(), String> {
    let mut value = || iter.next().ok_or_else(|| format!("{flag} needs a value"));
    match flag {
        "--bind" => args.bind = value()?,
        "--data-dir" => args.data_dir = PathBuf::from(value()?),
        "--config-dir" => args.config_dir = Some(PathBuf::from(value()?)),
        "--model" => args.model = Some(value()?),
        "--forge-url" => args.forge_url = Some(value()?),
        "--clone-base-url" => args.clone_base_url = Some(value()?),
        other => unreachable!("apply_flag admitted {other:?}"),
    }
    Ok(())
}

/// Flags naming an environment variable that *holds* the value rather than being it.
fn apply_token_env_flag(
    args: &mut Args,
    flag: &str,
    iter: &mut impl Iterator<Item = String>,
    env: &impl EnvLookup,
) -> Result<(), String> {
    let name = iter.next().ok_or_else(|| format!("{flag} needs a value"))?;
    let token = env.var(&name).ok_or_else(|| format!("{name} is not set"))?;
    match flag {
        "--forge-token-env" => args.forge_token = token,
        "--token-env" => args.token = token,
        other => unreachable!("apply_flag admitted {other:?}"),
    }
    Ok(())
}

fn usage(message: impl std::fmt::Display) -> String {
    format!(
        "{message}\n\nusage: liberado-worker [--bind ADDR] [--data-dir PATH] [--config-dir PATH]\n\
         \x20                [--model NAME] [--forge-url URL] [--forge-token-env VAR]\n\
         \x20                [--clone-base-url URL] [--token-env VAR] [--max-concurrent N]\n\
         \x20                [--question-timeout-secs S] [--max-open-questions N]\n\n\
         Env: LIBERADO_WORKER_BIND LIBERADO_WORKER_TOKEN LIBERADO_DATA_DIR\n\
         \x20\x20\x20\x20 LIBERADO_CONFIG_DIR LIBERADO_CODER_PROVIDER LIBERADO_FORGE_URL LIBERADO_FORGE_TOKEN"
    )
}

#[cfg(test)]
mod tests;
