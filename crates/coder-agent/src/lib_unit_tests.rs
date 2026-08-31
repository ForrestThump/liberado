use super::*;
use liberado_coder_core::{
    CoderRoleConfig, CoderRunConfig, CoderTask, CoderTrace, CommandPolicy, PathPolicy,
    ProgressPolicy, SandboxSpec, WorkspaceRef,
};
use liberado_provider::{CompletionResponse, MockProvider, ToolInvocation};
use serde_json::json;

/// Retrying a full disk reproduces the full disk. The budget is better spent saying so.
#[test]
fn an_infrastructure_failure_is_not_retried() {
    let msg = "FAILURE_CLASS: infrastructure\nFAILURE_SIGNATURE: sig\n\
               REPAIR_HINT: The build environment failed, not your change.";
    assert!(!is_retryable(&CoderError::Validation(msg.to_string())));
}

/// The guard must be narrow. An ordinary validation failure is still worth another attempt,
/// or this change quietly turns every recoverable run into a single-shot one.
#[test]
fn ordinary_validation_failures_are_still_retried() {
    assert!(is_retryable(&CoderError::Validation(
        "FAILURE_CLASS: command_failed\nFINDINGS:\n- cargo exited 101".to_string()
    )));
    assert!(
        !is_retryable(&CoderError::NoChanges),
        "a read-only exhausted attempt must not start another identical NoChanges retry"
    );
    assert!(
        is_stuck_error(&CoderError::NoChanges),
        "NoChanges is still stuck: the pack must ask a human, not treat it as a crash"
    );
}

/// A git repo with one committed file. Identity is set explicitly because `user.email` /
/// `user.name` exist on every dev machine and on no CI runner.
fn reviewable_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir.path())
            .output()
            .expect("git available");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    run(&["init", "--quiet"]);
    run(&["config", "user.email", "test@liberado.local"]);
    run(&["config", "user.name", "Test"]);
    run(&["config", "commit.gpgsign", "false"]);
    std::fs::write(dir.path().join("tracked.rs"), "fn old() {}\n").unwrap();
    run(&["add", "tracked.rs"]);
    run(&["commit", "--quiet", "-m", "base"]);
    dir
}

/// The critic used to be handed untracked file *names* and no content, so it reported that it
/// could not see the file — while 334 lines of it sat on disk. A review of a change that omits
/// its largest part is not a review.
#[tokio::test]
async fn the_critic_sees_the_content_of_a_new_file() {
    let dir = reviewable_repo();
    std::fs::write(dir.path().join("added.rs"), "fn brand_new() -> u8 { 7 }\n").unwrap();

    let diff = workspace_diff(&dir.path().to_string_lossy()).await.unwrap();
    assert!(diff.contains("added.rs"), "{diff}");
    assert!(
        diff.contains("fn brand_new() -> u8 { 7 }"),
        "the new file's content must reach the reviewer, not only its name: {diff}"
    );
}

/// Tracked edits must not be displaced by the untracked section.
#[tokio::test]
async fn the_critic_still_sees_tracked_edits() {
    let dir = reviewable_repo();
    std::fs::write(dir.path().join("tracked.rs"), "fn changed() {}\n").unwrap();
    std::fs::write(dir.path().join("added.rs"), "fn brand_new() {}\n").unwrap();

    let diff = workspace_diff(&dir.path().to_string_lossy()).await.unwrap();
    assert!(
        diff.contains("fn changed()"),
        "tracked edit missing: {diff}"
    );
    assert!(diff.contains("fn brand_new()"), "new file missing: {diff}");
}

/// A binary file must be named but not inlined — a stray NUL would corrupt the transcript the
/// reviewer reads.
#[tokio::test]
async fn a_binary_new_file_is_named_but_not_inlined() {
    let dir = reviewable_repo();
    std::fs::write(dir.path().join("blob.bin"), [0u8, 159, 146, 150]).unwrap();

    let diff = workspace_diff(&dir.path().to_string_lossy()).await.unwrap();
    assert!(diff.contains("blob.bin"), "{diff}");
    assert!(diff.contains("(binary)"), "{diff}");
    assert!(!diff.contains('\0'), "no NUL may reach the transcript");
}

/// A clean tree must still read as clean.
#[tokio::test]
async fn a_clean_tree_is_reported_as_an_empty_diff() {
    let dir = reviewable_repo();
    let diff = workspace_diff(&dir.path().to_string_lossy()).await.unwrap();
    assert_eq!(diff, "(empty diff)", "{diff}");
}

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
async fn mocked_loop_edits_workspace_and_reports_changed_file() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    let provider = Arc::new(MockProvider::with_script("mock", write_then_report()));
    let backend = LiberadoLoopBackend::new(provider);
    let result = backend.run(request(dir.path(), "HEAD")).await.unwrap();

    assert_eq!(result.outcome, Outcome::Succeeded);
    assert_eq!(result.files_changed, vec!["hello.txt"]);
}

/// Mocked end-to-end: hashline enabled → tool catalog offers `hashline_edit`, system
/// prompt includes guidance, and a precomputed-tag patch mutates the workspace.
#[tokio::test]
async fn mocked_loop_hashline_edit_and_prompt_wiring() {
    use liberado_coder_core::HashlineConfig;

    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    let seed = "alpha\nbeta\ngamma\n";
    std::fs::write(dir.path().join("notes.txt"), seed).unwrap();
    run(dir.path(), &["git", "add", "."]);
    run(dir.path(), &["git", "commit", "-m", "seed notes"]);

    let tag = liberado_coder_tools::hashline_compute_file_hash(seed, 6);
    let patch = format!("[notes.txt#{tag}]\nPUT 2.=2:\n+BETA\n");

    let provider = Arc::new(MockProvider::with_script(
        "mock",
        [
            CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "hl-1",
                "hashline_edit",
                json!({ "input": patch }),
            )]),
            CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "report-1",
                liberado_executor::SUBMIT_REPORT_TOOL,
                json!({
                    "outcome": "succeeded",
                    "summary": "Patched notes.txt via hashline",
                    "artifacts": ["notes.txt"],
                    "new_high_signal_facts": [],
                    "follow_up": null
                }),
            )]),
        ],
    ));
    let backend = LiberadoLoopBackend::new(provider.clone());
    let mut request = request(dir.path(), "HEAD");
    request.task.description = "Change beta to BETA in notes.txt using hashline.".into();
    request.config.hashline = HashlineConfig {
        enabled: true,
        hash_length: 6,
    };
    request.config.coder.prompt =
        Some("You are a coding agent. Use tools then submit_report.".into());

    let result = backend.run(request).await.unwrap();
    assert_eq!(result.outcome, Outcome::Succeeded);
    assert!(
        result.files_changed.iter().any(|p| p == "notes.txt"),
        "files_changed={:?}",
        result.files_changed
    );
    let after = std::fs::read_to_string(dir.path().join("notes.txt")).unwrap();
    assert_eq!(after, "alpha\nBETA\ngamma\n");

    // First completion request must advertise hashline_edit and carry prompt guidance.
    let first = provider
        .received_requests()
        .into_iter()
        .next()
        .expect("provider received a completion request");
    let tool_names: Vec<&str> = first.tools.iter().map(|t| t.name.as_str()).collect();
    assert!(
        tool_names.contains(&"hashline_edit"),
        "tools={tool_names:?}"
    );
    let system = first
        .messages
        .iter()
        .find(|m| m.role == liberado_provider::Role::System)
        .map(|m| m.content.as_str())
        .unwrap_or("");
    assert!(
        system.contains("Hashline edit mode") || system.contains("hashline_edit"),
        "system prompt missing hashline guidance:\n{system}"
    );
    assert!(
        system.contains('6') || system.contains("6-char"),
        "system prompt should mention configured hash length"
    );
}

#[tokio::test]
async fn mocked_loop_hashline_disabled_omits_tool_from_catalog() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    let provider = Arc::new(MockProvider::with_script("mock", write_then_report()));
    let backend = LiberadoLoopBackend::new(provider.clone());
    let mut request = request(dir.path(), "HEAD");
    request.config.hashline = liberado_coder_core::HashlineConfig {
        enabled: false,
        hash_length: 4,
    };
    backend.run(request).await.unwrap();
    let first = provider.received_requests().into_iter().next().unwrap();
    let tool_names: Vec<&str> = first.tools.iter().map(|t| t.name.as_str()).collect();
    assert!(
        !tool_names.contains(&"hashline_edit"),
        "hashline_edit must be absent when disabled; tools={tool_names:?}"
    );
}

#[tokio::test]
async fn backend_asks_provider_factory_for_coder_role_model() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    let provider = Arc::new(MockProvider::with_script("mock", write_then_report()));
    let calls = Arc::new(Mutex::new(Vec::new()));
    let backend = LiberadoLoopBackend::with_provider_factory(Arc::new(RecordingProviderFactory {
        provider,
        calls: calls.clone(),
    }));
    let mut request = request(dir.path(), "HEAD");
    request.config.coder.model = "deepseek/deepseek-v4-pro".to_string();

    backend.run(request).await.unwrap();

    assert_eq!(
        calls.lock().unwrap().as_slice(),
        &[("coder".to_string(), "deepseek/deepseek-v4-pro".to_string())]
    );
}

#[tokio::test]
async fn internal_git_status_is_not_blocked_by_model_command_policy() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    let provider = Arc::new(MockProvider::with_script("mock", write_then_report()));
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
    let provider = Arc::new(MockProvider::with_script("mock", write_then_report()));
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
}

#[tokio::test]
async fn an_unhandled_error_still_writes_its_trace() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());

    let script = [write_then_report()[0].clone()];
    let provider = Arc::new(MockProvider::with_script("mock", script));
    let backend = LiberadoLoopBackend::new(provider);
    let mut request = request(dir.path(), "HEAD");
    let traces = dir.path().join("traces");
    request.config.trace_dir = Some(traces.to_string_lossy().into_owned());
    request.config.critic.prompt = Some("Review the diff.".to_string());

    let err = backend
        .run(request)
        .await
        .expect_err("an unhandled provider error must still fail the run");
    assert!(
        err.to_string().contains("exhausted"),
        "wrong failure reproduced: {err}"
    );

    let written: Vec<_> = std::fs::read_dir(&traces)
        .expect("trace dir must exist even though the attempt died")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .collect();
    assert!(
        !written.is_empty(),
        "the attempt that crashed is the one whose trace matters most"
    );

    let trace: CoderTrace =
        serde_json::from_str(&std::fs::read_to_string(written[0].path()).unwrap()).unwrap();

    assert!(
        trace
            .events
            .iter()
            .any(|e| matches!(e, CoderEvent::ToolFinished { .. })),
        "the work the attempt did must survive its failure: {:?}",
        trace.events
    );
    let aborted = trace.events.iter().find_map(|e| match e {
        CoderEvent::SessionAborted { error, .. } => Some(error.clone()),
        _ => None,
    });
    assert!(
        aborted.is_some_and(|e| e.contains("exhausted")),
        "the trace must say what killed the attempt: {:?}",
        trace.events
    );
}

#[tokio::test]
async fn a_run_survives_a_trace_it_cannot_write() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    let provider = Arc::new(MockProvider::with_script("mock", write_then_report()));
    let backend = LiberadoLoopBackend::new(provider);
    let mut request = request(dir.path(), "HEAD");

    let elsewhere = tempfile::tempdir().unwrap();
    let blocker = elsewhere.path().join("not-a-dir");
    std::fs::write(&blocker, "in the way").unwrap();
    request.config.trace_dir = Some(blocker.join("traces").to_string_lossy().into_owned());

    let result = backend
        .run(request)
        .await
        .expect("an unwritable trace directory must not fail the run");
    assert_eq!(result.files_changed, vec!["hello.txt"]);
    assert!(
        result.trace_path.is_none(),
        "no trace was written, so the result must not claim one"
    );
}

#[tokio::test]
async fn a_handled_failure_is_not_relabelled_as_an_abort() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    let script = [CompletionResponse::tool_calls(vec![ToolInvocation::new(
        "report-1",
        liberado_executor::SUBMIT_REPORT_TOOL,
        json!({
            "outcome": "succeeded",
            "summary": "did nothing",
            "artifacts": [],
            "new_high_signal_facts": [],
            "follow_up": null
        }),
    )])];
    let provider = Arc::new(MockProvider::with_script("mock", script));
    let backend = LiberadoLoopBackend::new(provider);
    let mut request = request(dir.path(), "HEAD");
    let traces = dir.path().join("traces");
    request.config.trace_dir = Some(traces.to_string_lossy().into_owned());

    let _ = backend.run(request).await;

    let attempt_zero = std::fs::read_dir(&traces)
        .expect("trace dir")
        .filter_map(|e| e.ok())
        .find(|e| {
            let file_name = e.file_name();
            let name = file_name.to_string_lossy();
            name.contains("-attempt-0-") && name.ends_with(".json")
        })
        .expect("a handled failure still writes a trace");

    let trace: CoderTrace =
        serde_json::from_str(&std::fs::read_to_string(attempt_zero.path()).unwrap()).unwrap();
    assert!(
        trace
            .events
            .iter()
            .any(|e| matches!(e, CoderEvent::SessionFinished { .. })),
        "the body's own verdict must be what the trace records: {:?}",
        trace.events
    );
    assert!(
        !trace
            .events
            .iter()
            .any(|e| matches!(e, CoderEvent::SessionAborted { .. })),
        "a deliberate failure is not an abort: {:?}",
        trace.events
    );
}

#[tokio::test]
async fn trace_keeps_full_tool_args_regardless_of_the_live_stream_cap() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    let provider = Arc::new(MockProvider::with_script(
        "mock",
        [
            CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "write-1",
                "write_file",
                json!({"path": "hello.txt", "content": "abcdefghijklmnopqrstuvwxyz"}),
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
    request.config.progress.event_preview_max_chars = 12;

    let result = backend.run(request).await.unwrap();
    let trace_json = std::fs::read_to_string(result.trace_path.unwrap()).unwrap();
    let trace: CoderTrace = serde_json::from_str(&trace_json).unwrap();
    let args_preview = trace
        .events
        .iter()
        .find_map(|event| match event {
            CoderEvent::ToolStarted { args_preview, .. } => Some(args_preview),
            _ => None,
        })
        .expect("tool args preview");

    assert!(
        args_preview.contains("abcdefghijklmnopqrstuvwxyz"),
        "the trace must keep the whole argument the tool was actually called with, not the \
         first {} characters of it: {args_preview}",
        12
    );
    assert!(
        args_preview.chars().count() <= trace::TRACE_MAX_CHARS,
        "still bounded — by the trace's own ceiling, not the live stream's"
    );
}

#[tokio::test]
async fn coder_role_requires_resolved_max_turns() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    let provider = Arc::new(MockProvider::new("mock"));
    let backend = LiberadoLoopBackend::new(provider);
    let mut request = request(dir.path(), "HEAD");
    request.config.coder.max_turns = None;

    let err = backend.run(request).await.unwrap_err();

    assert!(matches!(err, CoderError::Setup(_)));
    assert!(err.to_string().contains("max_turns"));
}

#[tokio::test]
async fn configured_validation_gate_sets_notes_on_success() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    let provider = Arc::new(MockProvider::with_script("mock", write_then_report()));
    let backend = LiberadoLoopBackend::new(provider);
    let mut request = request(dir.path(), "HEAD");
    request.config.validation_command = Some(test_command("validation-ok"));

    let result = backend.run(request).await.unwrap();

    assert_eq!(result.outcome, Outcome::Succeeded);
    assert!(result.validation_notes.unwrap().contains("validate"));
}

#[tokio::test]
async fn verifier_paths_exist_fails_incomplete_success() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    let provider = Arc::new(MockProvider::with_script("mock", write_then_report()));
    let backend = LiberadoLoopBackend::new(provider);
    let mut request = request(dir.path(), "HEAD");
    request.config.progress.max_attempts = 1;
    request.config.verifiers = vec![liberado_coder_core::VerifierSpec::PathsExist {
        id: "must".into(),
        paths: vec!["missing_required.txt".into()],
    }];

    let err = backend.run(request).await.unwrap_err();
    assert!(matches!(err, CoderError::Validation(_)));
    assert!(err.to_string().contains("missing_required"));
}

#[tokio::test]
async fn configured_validation_gate_fails_run_on_failure() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    let provider = Arc::new(MockProvider::with_script("mock", write_then_report()));
    let backend = LiberadoLoopBackend::new(provider);
    let mut request = request(dir.path(), "HEAD");
    request.config.progress.max_attempts = 1;
    request.config.validation_command = Some(failing_test_command());

    let err = backend.run(request).await.unwrap_err();

    assert!(matches!(err, CoderError::Validation(_)));
    let msg = err.to_string();
    assert!(
        msg.contains("validate") || msg.contains("exited") || msg.contains("Completeness"),
        "unexpected validation message: {msg}"
    );
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
    let mut request = request(dir.path(), "HEAD");
    request.config.progress.max_attempts = 1;
    let err = backend.run(request).await.unwrap_err();
    assert!(matches!(err, CoderError::NoChanges));
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

fn test_command(message: &str) -> liberado_coder_core::CoderCommandConfig {
    #[cfg(windows)]
    {
        liberado_coder_core::CoderCommandConfig {
            program: "cmd".to_string(),
            args: vec!["/C".to_string(), format!("echo {message}")],
            env: Default::default(),
            timeout_secs: None,
            output_max_bytes: None,
        }
    }
    #[cfg(not(windows))]
    {
        liberado_coder_core::CoderCommandConfig {
            program: "sh".to_string(),
            args: vec!["-c".to_string(), format!("echo {message}")],
            env: Default::default(),
            timeout_secs: None,
            output_max_bytes: None,
        }
    }
}

fn failing_test_command() -> liberado_coder_core::CoderCommandConfig {
    #[cfg(windows)]
    {
        liberado_coder_core::CoderCommandConfig {
            program: "cmd".to_string(),
            args: vec![
                "/C".to_string(),
                "echo validation-failed >&2 && exit /B 1".to_string(),
            ],
            env: Default::default(),
            timeout_secs: None,
            output_max_bytes: None,
        }
    }
    #[cfg(not(windows))]
    {
        liberado_coder_core::CoderCommandConfig {
            program: "sh".to_string(),
            args: vec![
                "-c".to_string(),
                "echo validation-failed >&2; exit 1".to_string(),
            ],
            env: Default::default(),
            timeout_secs: None,
            output_max_bytes: None,
        }
    }
}
