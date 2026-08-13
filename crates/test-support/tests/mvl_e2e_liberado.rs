//! Liberado producer adapter for the MVL e2e oracle (layer 2a).
//!
//! Drives the production [`Executor`] request loop with [`MockProvider`] (no live LLM).
//! The executor append-flushes MVL at the provider boundary; this suite then points the
//! shipped path-based oracle at that **new** file. It must not reread the hand-authored
//! sample fixtures as if they were producer output.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use liberado_executor::{Budget, Executor, MvlSession, SUBMIT_REPORT_TOOL, Task, ToolRuntime};
use liberado_provider::{
    CompletionRequest, CompletionResponse, MockProvider, Provider, ProviderError, ToolDef,
    ToolInvocation,
};
use liberado_test_support::mvl_oracle::{
    ConformanceOpts, ConformanceRule, VerdictStatus, run_mvl_conformance,
};
use serde_json::json;

const SHOWN: &str = "hit-café-✓";

fn scratch_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("liberado-mvl-producer-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch");
    dir
}

struct FixedRuntime {
    tools: Vec<ToolDef>,
    result: String,
}

#[async_trait]
impl ToolRuntime for FixedRuntime {
    fn catalog(&self) -> Vec<ToolDef> {
        self.tools.clone()
    }
    async fn invoke(&self, _call: &ToolInvocation) -> Result<String, String> {
        Ok(self.result.clone())
    }
}

/// Provider that asserts the MVL file already contains a flushed `prompt` **inside** `complete`.
/// That is the append-flush / request-boundary contract: a log written only at process exit
/// would still be empty here.
struct FlushCheckProvider {
    inner: MockProvider,
    mvl_path: PathBuf,
    saw_prompt: Mutex<bool>,
}

#[async_trait]
impl Provider for FlushCheckProvider {
    fn model(&self) -> String {
        self.inner.model()
    }
    async fn complete(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        let text = std::fs::read_to_string(&self.mvl_path).unwrap_or_default();
        if text.contains("\"type\":\"prompt\"") {
            *self.saw_prompt.lock().unwrap() = true;
        }
        self.inner.complete(request).await
    }
}

#[tokio::test]
async fn liberado_producer_writes_append_flushed_mvl_then_oracle_judges() {
    let dir = scratch_dir();
    let mvl_path = dir.join("producer.mvl.jsonl");
    let exec_path = dir.join("producer.execution.jsonl");
    let sample =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/trace_contracts/sample.mvl.jsonl");
    assert_ne!(
        mvl_path, sample,
        "must not write over the hand-authored fixture"
    );

    let session = MvlSession::open(&mvl_path, Some(&exec_path), "producer-run").expect("open mvl");
    let inner = MockProvider::with_script(
        "mock",
        vec![
            CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "c-search",
                "search",
                json!({"q": "x"}),
            )]),
            CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "c-submit",
                SUBMIT_REPORT_TOOL,
                json!({
                    "outcome": "succeeded",
                    "summary": "found it",
                    "artifacts": []
                }),
            )]),
        ],
    );
    let provider = Arc::new(FlushCheckProvider {
        inner,
        mvl_path: mvl_path.clone(),
        saw_prompt: Mutex::new(false),
    });
    let exec = Executor::new(provider.clone(), Budget::new(6)).with_mvl(Arc::new(session));
    let runtime = FixedRuntime {
        tools: vec![ToolDef::new(
            "search",
            "Search notes",
            json!({"type":"object","properties":{"q":{"type":"string"}}}),
        )],
        result: SHOWN.to_string(),
    };

    let report = exec
        .execute(&runtime, Task::new("You are the coder.", "find the thing"))
        .await
        .expect("execute");
    assert_eq!(report.summary, "found it");

    // File exists and is non-empty **before** the oracle runs.
    let meta = std::fs::metadata(&mvl_path).expect("producer mvl written");
    assert!(
        meta.len() > 0,
        "producer must write a non-empty MVL file before the oracle"
    );
    let produced = std::fs::read_to_string(&mvl_path).unwrap();
    assert!(
        !produced.contains("fixture-run-1"),
        "producer must not reread fixtures/trace_contracts/sample.mvl.jsonl"
    );
    assert!(
        *provider.saw_prompt.lock().unwrap(),
        "prompt event must be durable inside provider.complete (append-flush at request boundary)"
    );

    let mut expected = std::collections::BTreeMap::new();
    expected.insert("c-search".into(), SHOWN.to_string());
    expected.insert("c-submit".into(), "report accepted".into());
    let opts = ConformanceOpts {
        execution_path: Some(exec_path),
        expected_content_shown: expected,
        kill_after_seq: None,
    };
    let judged = run_mvl_conformance(&mvl_path, &opts).expect("oracle");
    for rule in ConformanceRule::ALL {
        let v = judged.verdict(rule).expect("rule present");
        assert_eq!(v.status, VerdictStatus::Pass, "{rule:?} {}", v.detail);
    }
}

/// Producer cases are live. They must not treat the hand-authored sample as e2e output.
#[test]
fn liberado_producer_is_not_gated_and_does_not_reread_sample() {
    let src = include_str!("mvl_e2e_liberado.rs");
    assert!(
        !src.contains("ignore = \"backlog"),
        "Liberado producer cases must run now that 0.6 emission exists"
    );
    assert!(src.contains("FlushCheckProvider"));
    assert!(src.contains("MvlSession::open"));
    assert!(src.contains("sample.mvl.jsonl") && src.contains("must not reread"));
    let sample =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/trace_contracts/sample.mvl.jsonl");
    assert!(sample.is_file(), "must not delete the layer-0 fixture");
}
