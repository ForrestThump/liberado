//! Split from `coding_run.rs` for module-health boundaries.

use super::*;
use liberado_coder_agent::assemble::entry;
use liberado_coder_core::CommandPolicy;
use liberado_coder_sandbox::HostWorkspace;
use liberado_coder_tools::CodingToolRuntime;
use liberado_executor::ToolRuntime;
use liberado_provider::{CompletionRequest, CompletionResponse, Message, MockProvider};

const FOUR_TOOL_THINKING: &str = r#"
[coder]
offered_tools = ["read_file", "write_file", "edit_file", "run_command"]

[coder.coder]
model = "deepseek/deepseek-v4-flash"
temperature = 0.1
max_turns = 30
reasoning = "high"
"#;

#[test]
fn load_coder_tuning_rejects_an_invalid_section_instead_of_defaulting() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("tuning.toml"),
        "[coder.coder]\nmodel = \"x\"\nprompt = \"p\"\nmax_turns = 0\n",
    )
    .unwrap();

    let err = load_coder_tuning(Some(dir.path()))
        .expect_err("max_turns = 0 must not become ACP defaults");
    assert!(
        err.contains("invalid [coder]"),
        "the operator must see a load error, got: {err}"
    );
    assert!(
        err.contains("max_turns"),
        "the error must name the bad field, got: {err}"
    );
}

/// File → ACP loader → shared assembler → catalog + outbound completion body.
///
/// Compare 2 configured four tools and `reasoning = high`. The loader discarded the
/// section and the model saw 21 tools with no thinking. This is that file, on the
/// path Paseo actually uses.
#[tokio::test]
async fn a_four_tool_thinking_file_reaches_the_acp_completion_request() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("tuning.toml"), FOUR_TOOL_THINKING).unwrap();

    let tuning = load_coder_tuning(Some(dir.path())).expect("compare-2-shaped tuning must load");
    let assembled = assemble_production_run(
        &tuning,
        entry::acp_surface(
            CoderTask::new("d2", "price the models"),
            dir.path().to_path_buf(),
            None,
            Some(30),
            0,
            Vec::new(),
        ),
    );

    assert_eq!(
        assembled.request.config.offered_tools.as_deref(),
        Some(
            [
                "read_file".to_string(),
                "write_file".to_string(),
                "edit_file".to_string(),
                "run_command".to_string()
            ]
            .as_slice()
        )
    );
    assert_eq!(
        assembled.request.config.coder.reasoning.as_deref(),
        Some("high")
    );

    let workspace = HostWorkspace::new(dir.path(), CommandPolicy::default()).unwrap();
    let runtime =
        CodingToolRuntime::from_workspace(workspace, assembled.request.config.path_policy.clone())
            .with_offered_tools(assembled.request.config.offered_tools.clone());
    let names: Vec<String> = runtime.catalog().into_iter().map(|t| t.name).collect();
    assert_eq!(
        names,
        vec!["read_file", "write_file", "edit_file", "run_command"],
        "the model-offered coding catalog must be the four configured names"
    );

    let inner = Arc::new(MockProvider::with_script(
        "session-model",
        [CompletionResponse::text("ok")],
    ));
    let factory = role_factory(Arc::clone(&inner) as Arc<dyn Provider>);
    let provider = factory
        .provider_for("coder", &assembled.request.config.coder)
        .unwrap();
    provider
        .complete(CompletionRequest::new(vec![Message::user("price")]))
        .await
        .unwrap();
    assert_eq!(
        inner.last_request().and_then(|request| request.reasoning),
        Some("high".to_string()),
        "ACP must put the loaded role reasoning on the outbound completion request"
    );
}
