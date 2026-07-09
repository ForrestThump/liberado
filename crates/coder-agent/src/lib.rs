//! Executor-backed coding backend MVP.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use chrono::Utc;
use liberado_coder_core::{
    CoderBackend, CoderError, CoderEvent, CoderRunRequest, CoderRunResult, CoderTrace,
    LIBERADO_LOOP_BACKEND,
};
use liberado_coder_tools::CodingToolRuntime;
use liberado_common::Outcome;
use liberado_executor::{Budget, Executor, Task, ToolRuntime};
use liberado_provider::{Provider, ToolDef, ToolInvocation};
use serde_json::{Value, json};
use tokio::process::Command;

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
        let session_id = session_id(&request);
        let events = Arc::new(Mutex::new(vec![CoderEvent::SessionStarted {
            session_id: session_id.clone(),
            backend: self.name().to_string(),
            task_id: request.task.id.clone(),
            at: Utc::now(),
        }]));
        let max_turns = request.config.coder.max_turns.unwrap_or(30);
        let runtime = CodingToolRuntime::new(
            &request.workspace.root,
            request.config.command_policy.clone(),
            request.config.path_policy.clone(),
        )
        .map_err(|e| CoderError::Tool(e.to_string()))?;
        let runtime = TracingToolRuntime::new(runtime, events.clone());

        push_event(
            &events,
            CoderEvent::RoleStarted {
                role: "coder".to_string(),
                model: request.config.coder.model.clone(),
                at: Utc::now(),
            },
        );
        let task = Task::new(coder_instructions(&request).await?, coder_goal(&request));
        let executor = Executor::new(self.provider.clone(), Budget::new(max_turns));
        let report = executor
            .execute(&runtime, task)
            .await
            .map_err(|e| CoderError::Provider(e.to_string()))?;
        push_event(
            &events,
            CoderEvent::RoleFinished {
                role: "coder".to_string(),
                at: Utc::now(),
            },
        );
        push_event(
            &events,
            CoderEvent::ReportFiled {
                outcome: report.outcome,
                summary: report.summary.clone(),
                at: Utc::now(),
            },
        );

        let files_changed = changed_files(&request.workspace.root).await?;
        if files_changed.is_empty() && report.outcome != Outcome::Failed {
            push_event(
                &events,
                CoderEvent::LoopGuardTriggered {
                    guard: "no_changes".to_string(),
                    action: "fail_run".to_string(),
                    at: Utc::now(),
                },
            );
            push_event(
                &events,
                CoderEvent::SessionFinished {
                    outcome: Outcome::Failed,
                    at: Utc::now(),
                },
            );
            let _ = write_trace(&request, &session_id, snapshot_events(&events), None).await;
            return Err(CoderError::NoChanges);
        }
        for path in &files_changed {
            push_event(
                &events,
                CoderEvent::FileChanged {
                    path: path.clone(),
                    at: Utc::now(),
                },
            );
        }
        push_event(
            &events,
            CoderEvent::SessionFinished {
                outcome: report.outcome,
                at: Utc::now(),
            },
        );

        let mut result = CoderRunResult {
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
        };
        result.trace_path = write_trace(
            &request,
            &session_id,
            snapshot_events(&events),
            Some(result.clone()),
        )
        .await?;
        Ok(result)
    }
}

struct TracingToolRuntime {
    inner: CodingToolRuntime,
    events: Arc<Mutex<Vec<CoderEvent>>>,
}

impl TracingToolRuntime {
    fn new(inner: CodingToolRuntime, events: Arc<Mutex<Vec<CoderEvent>>>) -> Self {
        Self { inner, events }
    }
}

#[async_trait]
impl ToolRuntime for TracingToolRuntime {
    fn catalog(&self) -> Vec<ToolDef> {
        self.inner.catalog()
    }

    async fn invoke(&self, call: &ToolInvocation) -> Result<String, String> {
        push_event(
            &self.events,
            CoderEvent::ToolStarted {
                name: call.name.clone(),
                args_preview: preview_value(&call.arguments),
                at: Utc::now(),
            },
        );
        let result = self.inner.invoke(call).await;
        push_event(
            &self.events,
            CoderEvent::ToolFinished {
                name: call.name.clone(),
                ok: result.is_ok(),
                result_preview: match &result {
                    Ok(value) => preview_str(value),
                    Err(value) => preview_str(value),
                },
                at: Utc::now(),
            },
        );
        result
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

async fn changed_files(workspace_root: &str) -> Result<Vec<String>, CoderError> {
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(workspace_root)
        .output()
        .await
        .map_err(|e| CoderError::Backend(format!("git status: {e}")))?;
    if !output.status.success() {
        return Err(CoderError::Backend(format!(
            "git status exited {:?}: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout.lines().filter_map(parse_status_path).collect())
}

fn push_event(events: &Arc<Mutex<Vec<CoderEvent>>>, event: CoderEvent) {
    events
        .lock()
        .expect("coder event mutex poisoned")
        .push(event);
}

fn snapshot_events(events: &Arc<Mutex<Vec<CoderEvent>>>) -> Vec<CoderEvent> {
    events.lock().expect("coder event mutex poisoned").clone()
}

fn preview_value(value: &Value) -> String {
    preview_str(&value.to_string())
}

fn preview_str(value: &str) -> String {
    const MAX_PREVIEW_CHARS: usize = 500;
    value.chars().take(MAX_PREVIEW_CHARS).collect()
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

fn session_id(request: &CoderRunRequest) -> String {
    format!(
        "{}-attempt-{}-{}",
        safe_segment(&request.task.id),
        request.attempt,
        Utc::now().format("%Y%m%dT%H%M%S%.3fZ")
    )
}

fn safe_segment(value: &str) -> String {
    let segment = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();
    let segment = segment.trim_matches('-');
    if segment.is_empty() {
        "session".to_string()
    } else {
        segment.to_string()
    }
}

async fn write_trace(
    request: &CoderRunRequest,
    session_id: &str,
    events: Vec<CoderEvent>,
    mut result: Option<CoderRunResult>,
) -> Result<Option<String>, CoderError> {
    let Some(trace_dir) = &request.config.trace_dir else {
        return Ok(None);
    };
    let path = trace_file_path(trace_dir, session_id);
    let path_string = path.to_string_lossy().to_string();
    if let Some(result) = &mut result {
        result.trace_path = Some(path_string.clone());
    }
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|e| {
            CoderError::Setup(format!("create trace dir {}: {e}", parent.display()))
        })?;
    }
    let trace = CoderTrace {
        session_id: session_id.to_string(),
        request: request.clone(),
        events,
        result,
    };
    let bytes = serde_json::to_vec_pretty(&trace)
        .map_err(|e| CoderError::Backend(format!("serialize coder trace: {e}")))?;
    tokio::fs::write(&path, bytes)
        .await
        .map_err(|e| CoderError::Backend(format!("write coder trace {}: {e}", path.display())))?;
    Ok(Some(path_string))
}

fn trace_file_path(trace_dir: &str, session_id: &str) -> PathBuf {
    Path::new(trace_dir).join(format!("{session_id}.json"))
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
                trace_dir: None,
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
    async fn internal_git_status_is_not_blocked_by_model_command_policy() {
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
        let mut request = request(dir.path(), "HEAD");
        request.config.command_policy.deny = vec!["git status".to_string()];

        let result = backend.run(request).await.unwrap();
        assert_eq!(result.files_changed, vec!["hello.txt"]);
    }

    #[tokio::test]
    async fn writes_trace_when_trace_dir_is_configured() {
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
        let mut request = request(dir.path(), "HEAD");
        request.config.trace_dir = Some(dir.path().join("traces").to_string_lossy().to_string());

        let result = backend.run(request).await.unwrap();
        let trace_path = result.trace_path.as_ref().expect("trace path");
        let trace_json = std::fs::read_to_string(trace_path).unwrap();
        let trace: CoderTrace = serde_json::from_str(&trace_json).unwrap();

        assert_eq!(trace.result.unwrap().summary, "Wrote hello.txt");
        assert!(trace.events.iter().any(|event| {
            matches!(
                event,
                CoderEvent::FileChanged { path, .. } if path == "hello.txt"
            )
        }));
        assert!(trace.events.iter().any(|event| {
            matches!(
                event,
                CoderEvent::ToolStarted { name, .. } if name == "write_file"
            )
        }));
        assert!(trace.events.iter().any(|event| {
            matches!(
                event,
                CoderEvent::ToolFinished { name, ok: true, .. } if name == "write_file"
            )
        }));
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
