use super::*;
use liberado_coder_core::{
    CoderRoleConfig, CoderRunConfig, CoderTask, CommandPolicy, PathPolicy, ProgressPolicy,
    SandboxSpec, WorkspaceRef,
};
use liberado_provider::{CompletionResponse, MockProvider, ProviderError, ToolInvocation};
use serde_json::json;

fn role() -> CoderRoleConfig {
    CoderRoleConfig {
        model: "mock".to_string(),
        prompt_path: None,
        prompt: Some("Edit the repo and report when done.".to_string()),
        temperature: None,
        max_tokens: None,
        max_turns: Some(6),
        reasoning: None,
    }
}

fn disabled_role() -> CoderRoleConfig {
    CoderRoleConfig {
        model: "mock".to_string(),
        prompt_path: None,
        prompt: None,
        temperature: None,
        max_tokens: None,
        max_turns: Some(4),
        reasoning: None,
    }
}

fn request(root: &std::path::Path, base_ref: &str) -> CoderRunRequest {
    CoderRunRequest {
        task: CoderTask::new("task-1", "write hello.txt"),
        workspace: WorkspaceRef::new(root.to_string_lossy(), base_ref),
        config: CoderRunConfig {
            backend: LIBERADO_LOOP_BACKEND.to_string(),
            trace_dir: None,
            trace_formats: Vec::new(),
            planner: disabled_role(),
            coder: role(),
            critic: disabled_role(),
            gate: liberado_coder_core::CoderGateConfig::default(),
            repair: None,
            sandbox: SandboxSpec::HostLocal,
            command_policy: CommandPolicy::default(),
            validation_command: None,
            verifiers: Vec::new(),
            verify_policy: Default::default(),
            path_policy: PathPolicy::default(),
            progress: ProgressPolicy::default(),
            hashline: liberado_coder_core::HashlineConfig::default(),
            session_critic: Default::default(),
            prompt_dir: None,
            edit: Default::default(),
            workspace_build: Default::default(),
            offered_tools: None,
        },
        attempt: 0,
        prior_feedback: Vec::new(),
        strategist_directive: None,
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

struct RecordingProviderFactory {
    provider: Arc<dyn Provider>,
    calls: Arc<Mutex<Vec<(String, String)>>>,
}

impl CoderProviderFactory for RecordingProviderFactory {
    fn provider_for(
        &self,
        role: &str,
        config: &CoderRoleConfig,
    ) -> Result<Arc<dyn Provider>, CoderError> {
        self.calls
            .lock()
            .unwrap()
            .push((role.to_string(), config.model.clone()));
        Ok(self.provider.clone())
    }
}

fn write_then_report() -> [CompletionResponse; 2] {
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
    ]
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

#[tokio::test]
async fn read_only_stall_fails_without_mutation() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    let provider = Arc::new(MockProvider::with_script(
        "mock",
        [
            CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "l1",
                "list_files",
                json!({}),
            )]),
            CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "l2",
                "list_files",
                json!({}),
            )]),
            CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "l3",
                "list_files",
                json!({}),
            )]),
            CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "l4",
                "list_files",
                json!({}),
            )]),
            CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "report-1",
                liberado_executor::SUBMIT_REPORT_TOOL,
                json!({
                    "outcome": "succeeded",
                    "summary": "explored only",
                    "artifacts": [],
                    "new_high_signal_facts": [],
                    "follow_up": null
                }),
            )]),
        ],
    ));
    let backend = LiberadoLoopBackend::new(provider);
    let mut request = request(dir.path(), "HEAD");
    request.config.coder.max_turns = Some(10);
    request.config.progress.read_only_turn_limit = 2;
    request.config.progress.same_tool_limit = 100;
    request.config.progress.max_attempts = 1;

    let err = backend.run(request).await.unwrap_err();
    assert!(matches!(err, CoderError::NoChanges));
}

#[tokio::test]
async fn critic_accepts_diff() {
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
            CompletionResponse::text(r#"{"quality":"acceptable"}"#),
        ],
    ));
    let backend = LiberadoLoopBackend::new(provider);
    let mut request = request(dir.path(), "HEAD");
    request.config.critic = CoderRoleConfig {
        model: "mock-critic".to_string(),
        prompt_path: None,
        prompt: Some("Review the diff strictly.".to_string()),
        temperature: Some(0.0),
        max_tokens: Some(512),
        max_turns: None,
        reasoning: None,
    };

    let result = backend.run(request).await.unwrap();
    assert_eq!(result.outcome, Outcome::Succeeded);
    assert_eq!(result.critic_verdict, Some(CriticVerdict::Acceptable));
}

#[tokio::test]
async fn an_empty_critic_response_does_not_discard_the_run() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    let mut script = write_then_report().to_vec();
    script.push(CompletionResponse {
        content: None,
        tool_calls: Vec::new(),
        finish_reason: liberado_provider::FinishReason::Stop,
        usage: None,
    });
    let provider = Arc::new(MockProvider::with_script("mock", script));
    let backend = LiberadoLoopBackend::new(provider);
    let mut request = request(dir.path(), "HEAD");
    request.config.critic.prompt = Some("Review the diff strictly.".to_string());

    let result = backend
        .run(request)
        .await
        .expect("an absent reviewer must not fail a finished run");
    assert_eq!(result.outcome, Outcome::Succeeded);
    assert_eq!(
        result.critic_verdict, None,
        "silence must not be recorded as a verdict"
    );
    assert!(
        result.summary.contains("abstained"),
        "the abstention must be visible in the summary: {}",
        result.summary
    );
}

#[tokio::test]
async fn a_schema_rejecting_critic_falls_back_to_plain_json() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    let provider = Arc::new(MockProvider::with_script("mock", write_then_report()));
    provider.push_error(ProviderError::InvalidRequest(
        "This response_format type is unavailable now".to_string(),
    ));
    provider.push(CompletionResponse::text(r#"{"quality":"acceptable"}"#));
    let backend = LiberadoLoopBackend::new(Arc::clone(&provider) as Arc<dyn Provider>);
    let mut request = request(dir.path(), "HEAD");
    request.config.critic.prompt = Some("Review the diff strictly.".to_string());

    let result = backend
        .run(request)
        .await
        .expect("a critic format fallback must not discard a completed run");
    assert_eq!(result.outcome, Outcome::Succeeded);
    assert_eq!(result.critic_verdict, Some(CriticVerdict::Acceptable));

    let requests = provider.received_requests();
    assert_eq!(
        requests.len(),
        4,
        "two coder turns plus two critic attempts"
    );
    assert!(
        requests[2].has_json_schema(),
        "first critic request keeps the schema"
    );
    assert!(
        !requests[3].has_json_schema(),
        "fallback must request plain JSON after a schema rejection"
    );
}

#[tokio::test]
async fn an_unparseable_critic_response_abstains_rather_than_failing() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    let mut script = write_then_report().to_vec();
    script.push(CompletionResponse::text("Looks fine to me, ship it!"));
    let provider = Arc::new(MockProvider::with_script("mock", script));
    let backend = LiberadoLoopBackend::new(provider);
    let mut request = request(dir.path(), "HEAD");
    request.config.critic.prompt = Some("Review the diff strictly.".to_string());

    let result = backend.run(request).await.expect("must not fail the run");
    assert_eq!(result.outcome, Outcome::Succeeded);
    assert_eq!(result.critic_verdict, None);
}

#[tokio::test]
async fn a_real_revision_request_still_fails_the_attempt() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    let mut script = write_then_report().to_vec();
    script.push(CompletionResponse::text(
        r#"{"quality":"needs_revision","issues":["no tests"]}"#,
    ));
    let provider = Arc::new(MockProvider::with_script("mock", script));
    let backend = LiberadoLoopBackend::new(provider);
    let mut request = request(dir.path(), "HEAD");
    request.config.critic.prompt = Some("Review the diff strictly.".to_string());
    request.config.progress.max_attempts = 1;

    let result = backend.run(request).await.unwrap();
    assert_eq!(result.outcome, Outcome::Failed);
    assert!(matches!(
        result.critic_verdict,
        Some(CriticVerdict::NeedsRevision { .. })
    ));
}

#[tokio::test]
async fn critic_needs_revision_fails_final_attempt() {
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
            CompletionResponse::text(r#"{"quality":"needs_revision","issues":["missing tests"]}"#),
        ],
    ));
    let backend = LiberadoLoopBackend::new(provider);
    let mut request = request(dir.path(), "HEAD");
    request.config.progress.max_attempts = 1;
    request.config.critic = CoderRoleConfig {
        model: "mock-critic".to_string(),
        prompt_path: None,
        prompt: Some("Review the diff strictly.".to_string()),
        temperature: None,
        max_tokens: None,
        max_turns: None,
        reasoning: None,
    };

    let result = backend.run(request).await.unwrap();
    assert_eq!(result.outcome, Outcome::Failed);
    assert!(matches!(
        result.critic_verdict,
        Some(CriticVerdict::NeedsRevision { issues }) if issues.iter().any(|i| i.contains("tests"))
    ));
}

#[tokio::test]
async fn planner_runs_before_coder_when_configured() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    let provider = Arc::new(MockProvider::with_script(
        "mock",
        [
            CompletionResponse::text(
                r#"{"summary":"write hello","steps":["create hello.txt"],"likely_files":["hello.txt"],"risks":[]}"#,
            ),
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
    let calls = Arc::new(Mutex::new(Vec::new()));
    let backend = LiberadoLoopBackend::with_provider_factory(Arc::new(RecordingProviderFactory {
        provider: provider.clone(),
        calls: calls.clone(),
    }));
    let mut request = request(dir.path(), "HEAD");
    request.config.planner = CoderRoleConfig {
        model: "mock-planner".to_string(),
        prompt_path: None,
        prompt: Some("Plan the task briefly.".to_string()),
        temperature: Some(0.0),
        max_tokens: Some(512),
        max_turns: None,
        reasoning: None,
    };

    let result = backend.run(request).await.unwrap();
    assert_eq!(result.outcome, Outcome::Succeeded);
    let roles: Vec<String> = calls
        .lock()
        .unwrap()
        .iter()
        .map(|(role, _)| role.clone())
        .collect();
    assert_eq!(roles, vec!["planner".to_string(), "coder".to_string()]);
    let requests = provider.received_requests();
    assert!(requests.len() >= 2);
    let worker_user = requests[1]
        .messages
        .iter()
        .find(|m| m.role == liberado_provider::Role::User)
        .map(|m| m.content.as_str())
        .unwrap_or("");
    assert!(
        worker_user.contains("Planner plan") || worker_user.contains("hello.txt"),
        "worker should see plan context: {worker_user}"
    );
}

#[tokio::test]
async fn validation_failure_uses_signature_feedback_for_repair() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    let provider = Arc::new(MockProvider::with_script(
        "mock",
        [
            CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "write-1",
                "write_file",
                json!({"path": "notes.txt", "content": "incomplete\n"}),
            )]),
            CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "report-1",
                liberado_executor::SUBMIT_REPORT_TOOL,
                json!({
                    "outcome": "succeeded",
                    "summary": "claimed done",
                    "artifacts": ["notes.txt"],
                    "new_high_signal_facts": [],
                    "follow_up": null
                }),
            )]),
            CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "write-2",
                "write_file",
                json!({"path": "required.txt", "content": "ok\n"}),
            )]),
            CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "report-2",
                liberado_executor::SUBMIT_REPORT_TOOL,
                json!({
                    "outcome": "succeeded",
                    "summary": "fixed gates",
                    "artifacts": ["required.txt"],
                    "new_high_signal_facts": [],
                    "follow_up": null
                }),
            )]),
        ],
    ));
    let calls = Arc::new(Mutex::new(Vec::new()));
    let backend = LiberadoLoopBackend::with_provider_factory(Arc::new(RecordingProviderFactory {
        provider: provider.clone(),
        calls: calls.clone(),
    }));
    let mut request = request(dir.path(), "HEAD");
    request.config.progress.max_attempts = 2;
    request.config.verifiers = vec![liberado_coder_core::VerifierSpec::PathsExist {
        id: "must".into(),
        paths: vec!["required.txt".into()],
    }];
    request.config.repair = Some(CoderRoleConfig {
        model: "mock-repair".to_string(),
        prompt_path: None,
        prompt: Some("Repair: satisfy frozen verifiers.".to_string()),
        temperature: None,
        max_tokens: None,
        max_turns: Some(6),
        reasoning: None,
    });

    let result = backend.run(request).await.unwrap();
    assert_eq!(result.outcome, Outcome::Succeeded);
    assert!(result.files_changed.iter().any(|p| p.contains("required")));
    let roles: Vec<String> = calls
        .lock()
        .unwrap()
        .iter()
        .map(|(role, _)| role.clone())
        .collect();
    assert_eq!(roles, vec!["coder".to_string(), "repair".to_string()]);
    let requests = provider.received_requests();
    let repair_msgs = requests.last().map(|r| &r.messages).unwrap();
    let repair_blob = repair_msgs
        .iter()
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        repair_blob.contains("FAILURE_CLASS")
            || repair_blob.contains("missing_path")
            || repair_blob.contains("Repair focus"),
        "repair should see signature routing: {repair_blob}"
    );
}

#[tokio::test]
async fn a_no_changes_attempt_is_not_retried() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    let provider = Arc::new(MockProvider::with_script(
        "mock",
        [
            CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "report-1",
                liberado_executor::SUBMIT_REPORT_TOOL,
                json!({
                    "outcome": "succeeded",
                    "summary": "Done",
                    "artifacts": [],
                    "new_high_signal_facts": [],
                    "follow_up": null
                }),
            )]),
            CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "write-1",
                "write_file",
                json!({"path": "hello.txt", "content": "hello\n"}),
            )]),
            CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "report-2",
                liberado_executor::SUBMIT_REPORT_TOOL,
                json!({
                    "outcome": "succeeded",
                    "summary": "Wrote hello.txt on retry",
                    "artifacts": ["hello.txt"],
                    "new_high_signal_facts": [],
                    "follow_up": null
                }),
            )]),
        ],
    ));
    let calls = Arc::new(Mutex::new(Vec::new()));
    let backend = LiberadoLoopBackend::with_provider_factory(Arc::new(RecordingProviderFactory {
        provider,
        calls: calls.clone(),
    }));
    let mut request = request(dir.path(), "HEAD");
    request.config.progress.max_attempts = 2;
    request.config.repair = Some(CoderRoleConfig {
        model: "mock-repair".to_string(),
        prompt_path: None,
        prompt: Some("Repair: actually write the file.".to_string()),
        temperature: None,
        max_tokens: None,
        max_turns: Some(6),
        reasoning: None,
    });

    let err = backend.run(request).await.expect_err("NoChanges must stop");
    assert!(
        matches!(err, CoderError::NoChanges),
        "a read-only exhausted attempt must not start another identical retry: {err}"
    );
    let roles: Vec<String> = calls
        .lock()
        .unwrap()
        .iter()
        .map(|(role, _)| role.clone())
        .collect();
    assert_eq!(
        roles,
        vec!["coder".to_string()],
        "repair must not run after a NoChanges stall"
    );
}

#[tokio::test]
#[ignore = "requires OPENROUTER_API_KEY and network access"]
async fn openrouter_deepseek_live_coding_smoke() {
    use liberado_provider_openai_compat::OpenAiCompatibleProvider;

    let api_key = std::env::var("OPENROUTER_API_KEY").expect("OPENROUTER_API_KEY not set");
    let model = std::env::var("LIBERADO_CODER_LIVE_MODEL")
        .unwrap_or_else(|_| "deepseek/deepseek-v4-pro".to_string());
    let provider = Arc::new(
        OpenAiCompatibleProvider::new(
            api_key,
            &model,
            OpenAiCompatibleProvider::OPENROUTER_BASE_URL,
        )
        .with_extra_client_error_status(vec![402]),
    );

    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    let mut request = request(dir.path(), "HEAD");
    request.task.description =
        "Create a file named hello.txt containing exactly: hello from liberado\n".to_string();
    request.config.coder.model = model;
    request.config.coder.prompt = Some(
        "You are a careful autonomous coding agent. Inspect the workspace when useful, make the requested code or file edits with the available tools, then submit a concise success report."
            .to_string(),
    );
    request.config.coder.max_turns = Some(10);
    request.config.progress.event_preview_max_chars = 1_000;
    request.config.progress.max_attempts = 1;

    let backend = LiberadoLoopBackend::new(provider);
    let result = backend.run(request).await.unwrap();

    assert_eq!(result.outcome, Outcome::Succeeded);
    assert!(result.files_changed.iter().any(|path| path == "hello.txt"));
    let content = std::fs::read_to_string(dir.path().join("hello.txt")).unwrap();
    assert_eq!(
        content.trim_end_matches(['\r', '\n']),
        "hello from liberado"
    );
}

#[tokio::test]
#[ignore = "requires OPENROUTER_API_KEY and network access"]
async fn openrouter_deepseek_live_hashline_edit_smoke() {
    use liberado_coder_core::{HashlineConfig, VerifierSpec};
    use liberado_provider_openai_compat::OpenAiCompatibleProvider;

    let api_key = std::env::var("OPENROUTER_API_KEY").expect("OPENROUTER_API_KEY not set");
    let model = std::env::var("LIBERADO_CODER_LIVE_MODEL")
        .unwrap_or_else(|_| "deepseek/deepseek-v4-pro".to_string());
    let provider = Arc::new(
        OpenAiCompatibleProvider::new(
            api_key,
            &model,
            OpenAiCompatibleProvider::OPENROUTER_BASE_URL,
        )
        .with_extra_client_error_status(vec![402]),
    );

    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    let seed = "\
# greet helper
def greet(name):
    msg = \"Hello, \" + name
    print(msg)
    return msg

if __name__ == \"__main__\":
    greet(\"world\")
";
    std::fs::write(dir.path().join("greet.py"), seed).unwrap();
    run(dir.path(), &["git", "add", "."]);
    run(dir.path(), &["git", "commit", "-m", "seed greet.py"]);

    let mut request = request(dir.path(), "HEAD");
    request.task.id = "hashline-live-1".into();
    request.task.description = "\
In greet.py, change ONLY the message construction so it uses an f-string: \
msg = f\"Hi, {name}\" instead of string concatenation. Do not rewrite the whole file \
if you can avoid it. Keep the rest of the file (function structure, print, return, \
__main__) intact.\n"
        .to_string();
    request.task.success_criteria = vec![
        "greet.py uses f\"Hi, {name}\" (or equivalent f-string Hi greeting)".into(),
        "greet.py still defines def greet(name)".into(),
    ];
    request.config.coder.model = model.clone();
    request.config.coder.prompt = Some(
        "You are a careful autonomous coding agent. Hashline edit mode is ENABLED.\n\
         - read_file returns [path#TAG] and LINE:content anchors.\n\
         - Prefer hashline_edit for existing files: pass a patch with [path#TAG] and \
         PUT/CUT ops using + body rows. Re-read after every edit because the tag changes.\n\
         - write_file is only for brand-new files. edit_file/apply_patch are fallbacks.\n\
         - When done, submit_report with outcome=succeeded only if the file really changed."
            .to_string(),
    );
    request.config.coder.max_turns = Some(16);
    request.config.hashline = HashlineConfig {
        enabled: true,
        hash_length: 6,
    };
    request.config.progress.event_preview_max_chars = 2_000;
    request.config.progress.max_attempts = 2;
    request.config.progress.read_only_turn_limit = 6;
    request.config.trace_dir = Some(dir.path().join("traces").to_string_lossy().into_owned());
    request.config.verifiers = vec![VerifierSpec::ContentContains {
        id: "hi-fstring".into(),
        path: "greet.py".into(),
        must_include: vec!["Hi,".into()],
    }];

    let backend = LiberadoLoopBackend::new(provider);
    let result = match backend.run(request).await {
        Ok(r) => r,
        Err(e) => panic!("hashline live smoke backend error: {e:#}"),
    };

    eprintln!(
        "hashline live smoke: outcome={:?} summary={} files={:?} diagnostics={}",
        result.outcome, result.summary, result.files_changed, result.diagnostics
    );
    if let Some(path) = &result.trace_path {
        eprintln!("trace: {path}");
        if let Ok(raw) = std::fs::read_to_string(path) {
            let used_hashline = raw.contains("hashline_edit");
            let used_read = raw.contains("\"read_file\"") || raw.contains("read_file");
            eprintln!("trace tool hints: hashline_edit={used_hashline} read_file={used_read}");
            for line in raw.lines().take(80) {
                if line.contains("ToolStarted")
                    || line.contains("tool_started")
                    || line.contains("\"name\"")
                {
                    eprintln!("trace-line: {line}");
                }
            }
        }
    }

    let content = std::fs::read_to_string(dir.path().join("greet.py")).unwrap();
    eprintln!("--- greet.py after run ---\n{content}\n--- end ---");

    assert_eq!(
        result.outcome,
        Outcome::Succeeded,
        "expected success; summary={} validation={:?}",
        result.summary,
        result.validation_notes
    );
    assert!(
        result
            .files_changed
            .iter()
            .any(|p| p == "greet.py" || p.ends_with("greet.py")),
        "greet.py should be in files_changed: {:?}",
        result.files_changed
    );
    assert!(
        content.contains("def greet(name)"),
        "function signature must remain"
    );
    assert!(
        content.contains("Hi,") && content.contains("name"),
        "expected Hi greeting with name; got:\n{content}"
    );
    assert!(
        !content.contains("Hello, \" + name") && !content.contains("Hello, \"+ name"),
        "old concatenation should be gone; got:\n{content}"
    );
}

#[test]
fn parses_git_status_paths() {
    assert_eq!(
        gates::parse_status_path("?? hello.txt"),
        Some("hello.txt".to_string())
    );
    assert_eq!(
        gates::parse_status_path("R  old.txt -> new.txt"),
        Some("new.txt".to_string())
    );
    assert_eq!(gates::parse_status_path(""), None);
}

#[test]
fn parses_critic_json_with_fences() {
    let raw = "```json\n{\"quality\":\"acceptable\"}\n```";
    assert_eq!(
        critic::parse_critic_verdict(raw).unwrap(),
        CriticVerdict::Acceptable
    );
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
