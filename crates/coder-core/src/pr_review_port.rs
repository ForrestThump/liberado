//! One-shot PR-review process boundary.
//!
//! This port is separate from the repair [`crate::WorkerPort`]. It invokes one reviewer in an
//! already-pinned checkout and returns evidence. Admission, fallback, and forge policy stay with
//! shepherd.

use std::path::PathBuf;
use std::process::Stdio;

use crate::pr_review::{
    ReviewResult, WorkerFailure, classify_codex_failure, classify_opencode_failure,
    codex_review_args, cursor_review_args, grok_review_args, opencode_review_args,
    parse_opencode_success, parse_plain_success, review_prompt,
};

#[path = "pr_review_invoke.rs"]
mod invoke;
#[cfg(test)]
pub(crate) use invoke::is_forge_environment;
use invoke::{CapturedOutput, apply_review_env, capture, invoke_process, parse_codex_or_fail};
pub use invoke::{artifact_digest, serialize_review_result};

/// JSON Schema supplied to `codex exec review`.
pub const REVIEW_RESULT_SCHEMA: &str = include_str!("pr_review_schema.json");

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewInvokeRequest {
    pub task_id: String,
    pub command_id: String,
    pub run_id: String,
    pub repository: String,
    pub pr_number: u64,
    pub base_sha: String,
    pub expected_sha: String,
    pub workspace: PathBuf,
    pub schema_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewInvokeOutcome {
    Finished {
        result: ReviewResult,
        artifact_digest: String,
    },
    Unavailable {
        failure: WorkerFailure,
    },
    Failed {
        reason: String,
    },
    Stale {
        expected: String,
        observed: String,
    },
}

/// A single review invocation. It has no repair start/resume/collect lifecycle.
pub trait ReviewPort: Send + Sync {
    fn invoke(&self, request: &ReviewInvokeRequest) -> ReviewInvokeOutcome;
}

#[derive(Debug, Clone)]
pub struct CodexReviewPort {
    executable: PathBuf,
    extra_env: Vec<(String, String)>,
}

impl CodexReviewPort {
    pub fn new(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            extra_env: Vec::new(),
        }
    }

    #[cfg(all(test, unix))]
    fn with_env(mut self, name: &str, value: &str) -> Self {
        self.extra_env.push((name.into(), value.into()));
        self
    }
}

impl ReviewPort for CodexReviewPort {
    fn invoke(&self, request: &ReviewInvokeRequest) -> ReviewInvokeOutcome {
        invoke_process(
            request,
            || self.run(request),
            parse_codex_or_fail,
            classify_codex_failure,
        )
    }
}

#[derive(Debug, Clone)]
pub struct OpenCodeReviewPort {
    executable: PathBuf,
    model: String,
    extra_env: Vec<(String, String)>,
}

impl OpenCodeReviewPort {
    pub fn new(executable: impl Into<PathBuf>, model: impl Into<String>) -> Self {
        Self {
            executable: executable.into(),
            model: model.into(),
            extra_env: Vec::new(),
        }
    }

    #[cfg(all(test, unix))]
    fn with_env(mut self, name: &str, value: &str) -> Self {
        self.extra_env.push((name.into(), value.into()));
        self
    }

    fn run(&self, request: &ReviewInvokeRequest) -> Result<CapturedOutput, String> {
        let prompt = review_prompt(&request.base_sha, &request.expected_sha);
        let mut command = liberado_common::process::std_command(&self.executable);
        command
            .args(opencode_review_args(&self.model, &prompt))
            .current_dir(&request.workspace)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        apply_review_env(&mut command, &self.extra_env);
        capture(command)
    }
}

impl ReviewPort for OpenCodeReviewPort {
    fn invoke(&self, request: &ReviewInvokeRequest) -> ReviewInvokeOutcome {
        invoke_process(
            request,
            || self.run(request),
            parse_opencode_success,
            classify_opencode_failure,
        )
    }
}

/// Headless print CLI (Grok `--single`, Cursor `agent --print`). Native stdout is the review.
#[derive(Debug, Clone)]
pub struct PromptReviewPort {
    executable: PathBuf,
    extra_env: Vec<(String, String)>,
    args_for_prompt: fn(&str) -> Vec<String>,
}

impl PromptReviewPort {
    pub fn grok(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            extra_env: Vec::new(),
            args_for_prompt: grok_review_args,
        }
    }

    pub fn cursor(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            extra_env: Vec::new(),
            args_for_prompt: cursor_review_args,
        }
    }

    #[cfg(all(test, unix))]
    fn with_env(mut self, name: &str, value: &str) -> Self {
        self.extra_env.push((name.into(), value.into()));
        self
    }

    fn run(&self, request: &ReviewInvokeRequest) -> Result<CapturedOutput, String> {
        let prompt = review_prompt(&request.base_sha, &request.expected_sha);
        let mut command = liberado_common::process::std_command(&self.executable);
        command
            .args((self.args_for_prompt)(&prompt))
            .current_dir(&request.workspace)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        apply_review_env(&mut command, &self.extra_env);
        capture(command)
    }
}

impl ReviewPort for PromptReviewPort {
    fn invoke(&self, request: &ReviewInvokeRequest) -> ReviewInvokeOutcome {
        invoke_process(
            request,
            || self.run(request),
            parse_plain_success,
            classify_codex_failure,
        )
    }
}

impl CodexReviewPort {
    fn run(&self, request: &ReviewInvokeRequest) -> Result<CapturedOutput, String> {
        let schema = request.schema_path.to_string_lossy();
        let mut command = liberado_common::process::std_command(&self.executable);
        command
            .args(codex_review_args(
                &request.base_sha,
                &request.expected_sha,
                &schema,
            ))
            .current_dir(&request.workspace)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        apply_review_env(&mut command, &self.extra_env);
        capture(command)
    }
}

#[cfg(test)]
#[path = "pr_review_port_tests.rs"]
mod tests;
