//! Shared helpers for coder-agent integration tests (mock e2e + live scaffold).
//!
//! Keep this free of live network calls. Fixtures live in `tests/fixtures/`.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use liberado_coder_agent::LiberadoLoopBackend;
use liberado_coder_core::{
    CoderRoleConfig, CoderRunConfig, CoderRunRequest, CoderTask, CommandPolicy, FreezeAuthority,
    GoalContract, GoalContractDraft, IntakeOutcome, LIBERADO_LOOP_BACKEND, PathPolicy,
    ProgressPolicy, SandboxSpec, VerifierSpec, WorkspaceRef,
};
use liberado_provider::{CompletionResponse, MockProvider, Provider, ToolInvocation};
use serde_json::json;

/// Directory containing JSON intake fixtures (`tests/fixtures/`).
pub fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

pub fn load_fixture_text(name: &str) -> String {
    let path = fixtures_dir().join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!("failed to read fixture {}: {e}", path.display());
    })
}

pub fn load_intake_outcome(name: &str) -> IntakeOutcome {
    let text = load_fixture_text(name);
    serde_json::from_str(&text).unwrap_or_else(|e| {
        panic!("fixture {name} is not a valid IntakeOutcome: {e}\n{text}");
    })
}

pub fn intake_response(name: &str) -> CompletionResponse {
    CompletionResponse::text(load_fixture_text(name))
}

/// Structural-only draft used when tests construct contracts without going through fixtures.
pub fn structural_todo_draft() -> GoalContractDraft {
    GoalContractDraft {
        description: "Build a minimal todo CLI with add/list and a file store.".into(),
        success_criteria: vec![
            "Cargo.toml and src/main.rs exist".into(),
            "src/main.rs defines fn main".into(),
        ],
        verifiers: structural_todo_verifiers(),
        out_of_scope: vec!["network access".into(), "GUI".into()],
        assumed_defaults: vec!["Rust binary crate".into()],
        domain_hint: Some("coding".into()),
        // No verify_profile: mock e2e must not require cargo/network.
        verify_profile: None,
    }
}

pub fn structural_todo_verifiers() -> Vec<VerifierSpec> {
    vec![
        VerifierSpec::PathsExist {
            id: "required_paths".into(),
            paths: vec!["Cargo.toml".into(), "src/main.rs".into()],
        },
        VerifierSpec::ContentContains {
            id: "main_fn".into(),
            path: "src/main.rs".into(),
            must_include: vec!["fn main".into()],
        },
        VerifierSpec::GitNonemptyDiff {
            id: "has_diff".into(),
        },
    ]
}

pub fn freeze_structural_todo(id: &str) -> GoalContract {
    GoalContract::freeze(id, structural_todo_draft(), FreezeAuthority::Human).unwrap()
}

pub fn role(prompt: &str, max_turns: u32) -> CoderRoleConfig {
    CoderRoleConfig {
        model: "mock".into(),
        prompt_path: None,
        prompt: Some(prompt.into()),
        temperature: None,
        max_tokens: None,
        max_turns: Some(max_turns),
    }
}

pub fn disabled_role() -> CoderRoleConfig {
    CoderRoleConfig {
        model: "mock".into(),
        prompt_path: None,
        prompt: None,
        temperature: None,
        max_tokens: None,
        max_turns: Some(4),
    }
}

/// Base coding request shell (empty verifiers; contract apply stamps them).
pub fn base_request(root: &Path) -> CoderRunRequest {
    CoderRunRequest {
        // Empty id so `request_from_contract` stamps the frozen contract id.
        task: CoderTask::new("", "placeholder description"),
        workspace: WorkspaceRef::new(root.to_string_lossy(), "HEAD"),
        config: CoderRunConfig {
            backend: LIBERADO_LOOP_BACKEND.into(),
            trace_dir: None,
            planner: disabled_role(),
            coder: role(
                "Implement the frozen contract. Use tools, then submit a report.",
                8,
            ),
            critic: disabled_role(),
            gate: liberado_coder_core::CoderGateConfig::default(),
            repair: None,
            sandbox: SandboxSpec::HostLocal,
            command_policy: CommandPolicy::default(),
            validation_command: None,
            verifiers: Vec::new(),
            verify_policy: Default::default(),
            path_policy: PathPolicy::default(),
            progress: ProgressPolicy {
                max_attempts: 1,
                ..ProgressPolicy::default()
            },
        },
        attempt: 0,
        prior_feedback: Vec::new(),
        strategist_directive: None,
    }
}

pub fn init_repo(root: &Path) {
    run(root, &["git", "init"]);
    run(root, &["git", "config", "user.email", "test@example.com"]);
    run(root, &["git", "config", "user.name", "Test User"]);
    std::fs::write(root.join("README.md"), "# test\n").unwrap();
    run(root, &["git", "add", "."]);
    run(root, &["git", "commit", "-m", "base"]);
}

fn run(root: &Path, command: &[&str]) {
    let status = std::process::Command::new(command[0])
        .args(&command[1..])
        .current_dir(root)
        .status()
        .unwrap_or_else(|e| panic!("failed to spawn {command:?}: {e}"));
    assert!(status.success(), "command failed: {command:?}");
}

/// Build a mock worker script that satisfies frozen structural verifiers from a live draft.
/// Writes every `paths_exist` path (and content_contains targets) with plausible content.
pub fn write_contract_paths_then_report(
    contract: &liberado_coder_core::GoalContract,
) -> Vec<CompletionResponse> {
    use liberado_coder_core::VerifierSpec;
    use std::collections::BTreeMap;

    let mut path_content: BTreeMap<String, String> = BTreeMap::new();

    for v in &contract.draft.verifiers {
        match v {
            VerifierSpec::PathsExist { paths, .. } => {
                for p in paths {
                    path_content.entry(p.clone()).or_insert_with(|| {
                        if p.ends_with("Cargo.toml") {
                            r#"[package]
name = "todo-cli"
version = "0.1.0"
edition = "2021"
"#
                            .to_string()
                        } else if p.ends_with(".rs") {
                            "fn main() {\n    println!(\"todo-cli stub\");\n}\n".to_string()
                        } else {
                            format!("// scaffold for {p}\n")
                        }
                    });
                }
            }
            VerifierSpec::ContentContains {
                path, must_include, ..
            } => {
                let entry = path_content.entry(path.clone()).or_insert_with(|| {
                    if path.ends_with(".rs") {
                        "fn main() {}\n".to_string()
                    } else {
                        String::new()
                    }
                });
                for needle in must_include {
                    if !entry.contains(needle) {
                        entry.push_str(needle);
                        entry.push('\n');
                    }
                }
            }
            _ => {}
        }
    }

    if path_content.is_empty() {
        return write_todo_scaffold_then_report();
    }

    let mut responses = Vec::new();
    let artifacts: Vec<String> = path_content.keys().cloned().collect();
    for (i, (path, content)) in path_content.into_iter().enumerate() {
        responses.push(CompletionResponse::tool_calls(vec![ToolInvocation::new(
            format!("write-{i}"),
            "write_file",
            json!({"path": path, "content": content}),
        )]));
    }
    responses.push(CompletionResponse::tool_calls(vec![ToolInvocation::new(
        "report-1",
        liberado_executor::SUBMIT_REPORT_TOOL,
        json!({
            "outcome": "succeeded",
            "summary": "Scaffolded files from frozen contract",
            "artifacts": artifacts,
            "new_high_signal_facts": [],
            "follow_up": null
        }),
    )]));
    responses
}

/// Scripted worker that materializes the structural todo CLI scaffold.
pub fn write_todo_scaffold_then_report() -> Vec<CompletionResponse> {
    let cargo_toml = r#"[package]
name = "todo-cli"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "todo-cli"
path = "src/main.rs"
"#;
    let main_rs = r#"fn main() {
    println!("todo-cli stub");
}
"#;
    vec![
        CompletionResponse::tool_calls(vec![ToolInvocation::new(
            "mkdir-1",
            "write_file",
            json!({"path": "Cargo.toml", "content": cargo_toml}),
        )]),
        // write_file creates parents in coding tools; explicit src/main.rs is enough.
        CompletionResponse::tool_calls(vec![ToolInvocation::new(
            "write-main",
            "write_file",
            json!({"path": "src/main.rs", "content": main_rs}),
        )]),
        CompletionResponse::tool_calls(vec![ToolInvocation::new(
            "report-1",
            liberado_executor::SUBMIT_REPORT_TOOL,
            json!({
                "outcome": "succeeded",
                "summary": "Scaffolded minimal todo CLI",
                "artifacts": ["Cargo.toml", "src/main.rs"],
                "new_high_signal_facts": [],
                "follow_up": null
            }),
        )]),
    ]
}

/// Incomplete worker: only writes README note — should fail structural gates.
pub fn write_incomplete_then_report() -> Vec<CompletionResponse> {
    vec![
        CompletionResponse::tool_calls(vec![ToolInvocation::new(
            "write-1",
            "write_file",
            json!({"path": "notes.txt", "content": "I started but did not finish\n"}),
        )]),
        CompletionResponse::tool_calls(vec![ToolInvocation::new(
            "report-1",
            liberado_executor::SUBMIT_REPORT_TOOL,
            json!({
                "outcome": "succeeded",
                "summary": "Claimed done without scaffold",
                "artifacts": ["notes.txt"],
                "new_high_signal_facts": [],
                "follow_up": null
            }),
        )]),
    ]
}

pub fn mock_backend(
    responses: impl IntoIterator<Item = CompletionResponse>,
) -> LiberadoLoopBackend {
    LiberadoLoopBackend::new(Arc::new(MockProvider::with_script("mock", responses)))
}

pub fn mock_provider(responses: impl IntoIterator<Item = CompletionResponse>) -> Arc<MockProvider> {
    Arc::new(MockProvider::with_script("mock", responses))
}

/// Live OpenRouter provider when env is set; panics with a clear message otherwise.
pub fn openrouter_provider_from_env(model: &str) -> Arc<dyn Provider> {
    use liberado_provider_openai_compat::OpenAiCompatibleProvider;

    let api_key = std::env::var("OPENROUTER_API_KEY")
        .expect("OPENROUTER_API_KEY must be set for ignored live scaffold tests");
    Arc::new(
        OpenAiCompatibleProvider::new(
            api_key,
            model,
            OpenAiCompatibleProvider::OPENROUTER_BASE_URL,
        )
        .with_extra_client_error_status(vec![402]),
    )
}

pub fn live_model() -> String {
    std::env::var("LIBERADO_CODER_LIVE_MODEL")
        .unwrap_or_else(|_| "deepseek/deepseek-v4-pro".to_string())
}

/// Drop command verifiers (and cargo profiles) so a mock worker can satisfy a live intake draft.
pub fn structural_only(draft: &mut GoalContractDraft) {
    draft.verify_profile = None;
    draft
        .verifiers
        .retain(|v| !matches!(v, VerifierSpec::Command { .. }));
    // Ensure at least a nonempty-diff gate so "success with no work" still fails.
    if !draft
        .verifiers
        .iter()
        .any(|v| matches!(v, VerifierSpec::GitNonemptyDiff { .. }))
    {
        draft.verifiers.push(VerifierSpec::GitNonemptyDiff {
            id: "has_diff".into(),
        });
    }
}
