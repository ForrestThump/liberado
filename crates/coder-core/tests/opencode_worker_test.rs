use liberado_coder_core::{
    ControlPlaneSupervisor, DispatchTaskRequest, OpenCodeWorker, OpenCodeWorkerConfig,
    TaskEventKind, WorkerStatus,
};
use std::process::Command;
use std::sync::Arc;
use tempfile::TempDir;

#[test]
fn opencode_worker_constructs_with_custom_config() {
    let config = OpenCodeWorkerConfig {
        executable: Some("custom-opencode".into()),
        model: "openrouter/~deepseek/deepseek-v4-flash-latest".into(),
        auto_approve: false,
    };
    let worker = OpenCodeWorker::new(config.clone());
    assert_eq!(worker.config().model, config.model);
    assert!(!worker.config().auto_approve);
    assert_eq!(
        worker.config().executable.as_deref(),
        Some("custom-opencode")
    );
}

#[test]
#[ignore = "requires local opencode CLI and network access to OpenRouter"]
fn test_live_opencode_dispatch() {
    // 1. Set up temporary git repository
    let temp = TempDir::new().expect("create temp dir");
    let temp_path = temp.path();

    // Initialize git repo and identity
    Command::new("git")
        .args(["init"])
        .current_dir(temp_path)
        .status()
        .expect("git init");

    Command::new("git")
        .args(["config", "user.name", "Liberado Agent"])
        .current_dir(temp_path)
        .status()
        .expect("git config name");

    Command::new("git")
        .args(["config", "user.email", "agent@liberado.internal"])
        .current_dir(temp_path)
        .status()
        .expect("git config email");

    // Write initial README file
    std::fs::write(
        temp_path.join("README.md"),
        "# Sample Project\n\nInitial content.\n",
    )
    .expect("write README");

    Command::new("git")
        .args(["add", "README.md"])
        .current_dir(temp_path)
        .status()
        .expect("git add");

    Command::new("git")
        .args(["commit", "-m", "Initial commit"])
        .current_dir(temp_path)
        .status()
        .expect("git commit");

    // 2. Configure OpenCode worker and Supervisor
    let config = OpenCodeWorkerConfig {
        executable: None,
        model: "openrouter/~deepseek/deepseek-v4-flash-latest".into(),
        auto_approve: true,
    };
    let worker = Arc::new(OpenCodeWorker::new(config));
    let supervisor = ControlPlaneSupervisor::new(worker);

    let req = DispatchTaskRequest::new(
        "task-live-1",
        "Add a CONTRIBUTING.md file detailing contribution guidelines",
        temp_path.to_string_lossy().to_string(),
        "feat/contributing",
        "master",
    )
    .with_acceptance_criteria(vec!["Create CONTRIBUTING.md with PR guidelines".into()]);

    // 3. Dispatch task to OpenCode
    let (mut ledger, result) = supervisor.dispatch_task(&req).expect("dispatch task");

    println!("Live OpenCode execution status: {:?}", result.status);
    println!("Summary: {}", result.summary);
    println!("Commits: {:?}", result.commits);
    println!("Files changed: {:?}", result.files_changed);

    assert_eq!(result.status, WorkerStatus::Completed);
    let record = ledger.project().expect("projection");
    assert_eq!(record.task_id, "task-live-1");
    assert_eq!(record.prior_worker.as_deref(), Some("opencode"));
    assert!(record.external_session_id.is_some());

    // 4. Test CI kickback repair flow
    let repair_result = supervisor
        .handle_ci_failure(
            &mut ledger,
            vec!["Lint failure: CONTRIBUTING.md missing Code of Conduct section".into()],
            Some("CONTRIBUTING.md: check failed: no Code of Conduct section found".into()),
        )
        .expect("handle ci failure");

    println!("Live CI repair status: {:?}", repair_result.status);
    println!("Repair summary: {}", repair_result.summary);

    let _updated_record = ledger.project().expect("projection");
    assert!(
        ledger
            .events()
            .iter()
            .any(|e| matches!(e.payload, TaskEventKind::WorkerResumed { .. }))
    );
}
