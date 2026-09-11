//! Frozen reviewer argv and captured-output classification.

use super::{WorkerFailure, review_prompt};
use crate::ReviewWorkerConfig;

pub const CODEX_USAGE_LIMIT: &str = "ERROR: You've hit your usage limit";
const CODEX_USAGE_MARKERS: &[&str] = &[
    CODEX_USAGE_LIMIT,
    "You've hit your usage limit",
    "You've reached your weekly limit",
    "usage limit reached",
];

/// Classify only stable, captured signals. Exit status alone never selects fallback.
pub fn classify_codex_failure(output: &str) -> WorkerFailure {
    if CODEX_USAGE_MARKERS
        .iter()
        .any(|marker| output.contains(marker))
    {
        WorkerFailure::Exhausted
    } else if output.contains("rate limit") || output.contains("429") {
        WorkerFailure::RateLimited
    } else if output.contains("authentication") || output.contains("Unauthorized") {
        WorkerFailure::Auth
    } else if output.contains("permission denied") || output.contains("Permission denied") {
        WorkerFailure::Permission
    } else if output.contains("timed out") || output.contains("timeout") {
        WorkerFailure::Timeout
    } else {
        WorkerFailure::ModelFailure
    }
}

/// OpenCode review failures use captured text only. Unknown quota wording stays a failure.
pub fn classify_opencode_failure(output: &str) -> WorkerFailure {
    classify_codex_failure(output)
}

/// Frozen Codex command. It is evidence for Slice 2; no Slice 0/1 caller starts it.
pub fn codex_review_args(base_sha: &str, head_sha: &str, schema_path: &str) -> Vec<String> {
    vec![
        "exec".into(),
        "review".into(),
        "--sandbox".into(),
        "read-only".into(),
        "--json".into(),
        "--output-schema".into(),
        schema_path.into(),
        "--base".into(),
        base_sha.into(),
        "--commit".into(),
        head_sha.into(),
    ]
}

pub fn grok_review_args(schema_path: &str) -> Vec<String> {
    vec![
        "review".into(),
        "--headless".into(),
        "--schema".into(),
        schema_path.into(),
    ]
}

pub fn antigravity_review_args(schema_path: &str) -> Vec<String> {
    vec![
        "--print".into(),
        "--output-format".into(),
        "stream-json".into(),
        "--schema".into(),
        schema_path.into(),
    ]
}

pub fn cursor_review_args() -> Vec<String> {
    vec!["--mode".into(), "ask".into(), "--print".into()]
}

/// Frozen OpenCode command. It does not pass `--auto`, so writes stay denied.
pub fn opencode_review_args(model: &str, schema_path: &str, prompt: &str) -> Vec<String> {
    vec![
        "run".into(),
        "--format".into(),
        "json".into(),
        "--model".into(),
        model.into(),
        "--dir".into(),
        ".".into(),
        format!(
            "{prompt} The required result schema is in {schema_path}. Return only that JSON object."
        ),
    ]
}

/// Disabled workers yield no argv. Slices 0/1 never spawn the returned command.
pub fn review_process_argv(worker: &ReviewWorkerConfig, schema_path: &str) -> Option<Vec<String>> {
    if !worker.enabled() {
        return None;
    }
    Some(match worker {
        ReviewWorkerConfig::GrokBuild { executable, .. } => {
            prepend_exe(executable, grok_review_args(schema_path))
        }
        ReviewWorkerConfig::Codex { executable, .. } => {
            prepend_exe(executable, codex_review_args("base", "head", schema_path))
        }
        ReviewWorkerConfig::Antigravity { executable, .. } => {
            prepend_exe(executable, antigravity_review_args(schema_path))
        }
        ReviewWorkerConfig::CursorLocal { executable, .. } => {
            prepend_exe(executable, cursor_review_args())
        }
        ReviewWorkerConfig::OpenCode {
            executable, model, ..
        } => prepend_exe(
            executable,
            opencode_review_args(model, schema_path, &review_prompt("base", "head")),
        ),
        ReviewWorkerConfig::OpenaiCompatible { .. } => return None,
    })
}

fn prepend_exe(executable: &str, args: Vec<String>) -> Vec<String> {
    let mut command = vec![executable.to_string()];
    command.extend(args);
    command
}
