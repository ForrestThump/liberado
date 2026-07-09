//! Executor-backed coding backend MVP.

use std::sync::Arc;

use async_trait::async_trait;
use liberado_coder_core::{
    CoderBackend, CoderError, CoderRunRequest, CoderRunResult, LIBERADO_LOOP_BACKEND,
};
use liberado_coder_tools::CodingToolRuntime;
use liberado_common::Outcome;
use liberado_executor::{Budget, Executor, Task};
use liberado_provider::Provider;
use serde_json::json;

#[derive(Clone)]
pub struct LiberadoLoopBackend {
    provider: Arc<dyn Provider>,
}

impl LiberadoLoopBackend {
    pub fn new(provider: Arc<dyn Provider>) -> Self {
        Self { provider }
    }
}

#[async_trait]
impl CoderBackend for LiberadoLoopBackend {
    fn name(&self) -> &str {
        LIBERADO_LOOP_BACKEND
    }

    async fn run(&self, request: CoderRunRequest) -> Result<CoderRunResult, CoderError> {
        let max_turns = request.config.coder.max_turns.unwrap_or(30);
        let runtime = CodingToolRuntime::new(
            &request.workspace.root,
            request.config.command_policy.clone(),
            request.config.path_policy.clone(),
        )
        .map_err(|e| CoderError::Tool(e.to_string()))?;

        let task = Task::new(coder_instructions(&request).await?, coder_goal(&request));
        let executor = Executor::new(self.provider.clone(), Budget::new(max_turns));
        let report = executor
            .execute(&runtime, task)
            .await
            .map_err(|e| CoderError::Provider(e.to_string()))?;

        let files_changed = changed_files(&runtime).await?;
        if files_changed.is_empty() && report.outcome != Outcome::Failed {
            return Err(CoderError::NoChanges);
        }

        Ok(CoderRunResult {
            backend: self.name().to_string(),
            outcome: report.outcome,
            summary: report.summary,
            files_changed,
            validation_notes: None,
            critic_verdict: None,
            trace_path: None,
            diagnostics: json!({
                "artifacts_reported": report.artifacts,
                "attempt": request.attempt,
            }),
        })
    }
}

async fn coder_instructions(request: &CoderRunRequest) -> Result<String, CoderError> {
    if let Some(prompt) = request.config.coder.prompt.clone() {
        return Ok(prompt);
    }
    if let Some(path) = &request.config.coder.prompt_path {
        return tokio::fs::read_to_string(path)
            .await
            .map_err(|e| CoderError::Setup(format!("read coder prompt_path {path}: {e}")));
    }
    Err(CoderError::Setup(
        "coder role requires prompt or prompt_path".to_string(),
    ))
}

fn coder_goal(request: &CoderRunRequest) -> String {
    let mut goal = format!("Task: {}", request.task.description);
    if let Some(context) = &request.task.context {
        goal.push_str("\n\nContext:\n");
        goal.push_str(context);
    }
    if !request.task.success_criteria.is_empty() {
        goal.push_str("\n\nSuccess criteria:\n");
        for criterion in &request.task.success_criteria {
            goal.push_str("- ");
            goal.push_str(criterion);
            goal.push('\n');
        }
    }
    if !request.prior_feedback.is_empty() {
        goal.push_str("\n\nPrior feedback:\n");
        for feedback in &request.prior_feedback {
            goal.push_str("- ");
            goal.push_str(feedback);
            goal.push('\n');
        }
    }
    goal
}

async fn changed_files(runtime: &CodingToolRuntime) -> Result<Vec<String>, CoderError> {
    let output = runtime
        .invoke_json_for_backend(
            "run_command",
            json!({
                "program": "git",
                "args": ["status", "--porcelain"],
            }),
        )
        .await
        .map_err(|e| CoderError::Tool(e.to_string()))?;
    let stdout = output
        .get("stdout")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    Ok(stdout.lines().filter_map(parse_status_path).collect())
}

fn parse_status_path(line: &str) -> Option<String> {
    if line.len() < 4 {
        return None;
    }
    let path = line.get(3..)?.trim();
    if path.is_empty() {
        return None;
    }
    let path = path
        .rsplit_once(" -> ")
        .map(|(_, new_path)| new_path)
        .unwrap_or(path);
    Some(path.trim_matches('"').to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_coder_core::{
        CoderRoleConfig, CoderRunConfig, CoderTask, CommandPolicy, PathPolicy, ProgressPolicy,
        SandboxSpec, WorkspaceRef,
    };
    use liberado_provider::{CompletionResponse, MockProvider, ToolInvocation};

    fn role() -> CoderRoleConfig {
        CoderRoleConfig {
            model: "mock".to_string(),
            prompt_path: None,
            prompt: Some("Edit the repo and report when done.".to_string()),
            temperature: None,
            max_tokens: None,
            max_turns: Some(6),
        }
    }

    fn request(root: &std::path::Path, base_ref: &str) -> CoderRunRequest {
        CoderRunRequest {
            task: CoderTask::new("task-1", "write hello.txt"),
            workspace: WorkspaceRef::new(root.to_string_lossy(), base_ref),
            config: CoderRunConfig {
                backend: LIBERADO_LOOP_BACKEND.to_string(),
                planner: role(),
                coder: role(),
                critic: role(),
                repair: None,
                sandbox: SandboxSpec::HostLocal,
                command_policy: CommandPolicy::default(),
                path_policy: PathPolicy::default(),
                progress: ProgressPolicy::default(),
            },
            attempt: 0,
            prior_feedback: Vec::new(),
        }
    }

    fn request_with_role(
        root: &std::path::Path,
        base_ref: &str,
        coder: CoderRoleConfig,
    ) -> CoderRunRequest {
        let mut request = request(root, base_ref);
        request.config.coder = coder;
        request
    }

    #[tokio::test]
    async fn mocked_loop_edits_workspace_and_reports_changed_file() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let provider = Arc::new(MockProvider::with_script(
            "mock",
            [
                CompletionResponse::tool_calls(vec![ToolInvocation::new(
                    "write-1",
                    "write_file",
                    json!({"path": "hello.txt", "content": "hello\n"}),
                )]),
                CompletionResponse::tool_calls(vec![ToolInvocation::new(
                    "report-1",
                    liberado_executor::SUBMIT_REPORT_TOOL,
                    json!({
                        "outcome": "succeeded",
                        "summary": "Wrote hello.txt",
                        "artifacts": ["hello.txt"],
                        "new_high_signal_facts": [],
                        "follow_up": null
                    }),
                )]),
            ],
        ));
        let backend = LiberadoLoopBackend::new(provider);
        let result = backend.run(request(dir.path(), "HEAD")).await.unwrap();

        assert_eq!(result.outcome, Outcome::Succeeded);
        assert_eq!(result.files_changed, vec!["hello.txt"]);
    }

    #[tokio::test]
    async fn success_report_without_diff_is_no_changes() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let provider = Arc::new(MockProvider::with_script(
            "mock",
            [CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "report-1",
                liberado_executor::SUBMIT_REPORT_TOOL,
                json!({
                    "outcome": "succeeded",
                    "summary": "Done",
                    "artifacts": [],
                    "new_high_signal_facts": [],
                    "follow_up": null
                }),
            )])],
        ));
        let backend = LiberadoLoopBackend::new(provider);
        let err = backend.run(request(dir.path(), "HEAD")).await.unwrap_err();
        assert!(matches!(err, CoderError::NoChanges));
    }

    #[tokio::test]
    async fn loads_coder_prompt_from_prompt_path() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let prompt_path = dir.path().join("coder.md");
        std::fs::write(&prompt_path, "Prompt loaded from disk.").unwrap();
        let mut coder = role();
        coder.prompt = None;
        coder.prompt_path = Some(prompt_path.to_string_lossy().to_string());

        let provider = Arc::new(MockProvider::with_script(
            "mock",
            [CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "report-1",
                liberado_executor::SUBMIT_REPORT_TOOL,
                json!({
                    "outcome": "failed",
                    "summary": "No edit requested",
                    "artifacts": [],
                    "new_high_signal_facts": [],
                    "follow_up": null
                }),
            )])],
        ));
        let backend = LiberadoLoopBackend::new(provider.clone());
        let result = backend
            .run(request_with_role(dir.path(), "HEAD", coder))
            .await
            .unwrap();

        assert_eq!(result.outcome, Outcome::Failed);
        let sent = provider.last_request().unwrap();
        assert!(
            sent.messages[0]
                .content
                .contains("Prompt loaded from disk.")
        );
    }

    #[test]
    fn parses_git_status_paths() {
        assert_eq!(
            parse_status_path("?? hello.txt"),
            Some("hello.txt".to_string())
        );
        assert_eq!(
            parse_status_path("R  old.txt -> new.txt"),
            Some("new.txt".to_string())
        );
        assert_eq!(parse_status_path(""), None);
    }

    fn init_repo(root: &std::path::Path) {
        run(root, &["git", "init"]);
        run(root, &["git", "config", "user.email", "test@example.com"]);
        run(root, &["git", "config", "user.name", "Test User"]);
        std::fs::write(root.join("README.md"), "# test\n").unwrap();
        run(root, &["git", "add", "."]);
        run(root, &["git", "commit", "-m", "base"]);
    }

    fn run(root: &std::path::Path, command: &[&str]) {
        let status = std::process::Command::new(command[0])
            .args(&command[1..])
            .current_dir(root)
            .status()
            .unwrap();
        assert!(status.success(), "command failed: {command:?}");
    }
}
