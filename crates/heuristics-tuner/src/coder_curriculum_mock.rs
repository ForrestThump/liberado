//! CI-safe **mock** coder curriculum: scripted [`MockProvider`] runs through real
//! `LiberadoLoopBackend` workspaces for smoke + core scenarios.
//!
//! Live / hybrid ladder stays opt-in (`#[ignore]` / OpenRouter). This module is the
//! regression bar for scratchpad slice **D** — always green without API keys.

use std::sync::Arc;

use liberado_provider::{CompletionResponse, MockProvider, ToolInvocation};
use serde_json::json;

use crate::coder_scenarios::{
    CoderScenario, CoderTier, DEFAULT_CODER_SYSTEM_PROMPT, coder_scenarios_for,
};
use crate::coder_scoring::{CoderFitness, score_coder_candidate};
use crate::search::Budget;

/// Scripted tool loop for a named curriculum scenario. `None` if no mock script yet
/// (stress/greenfield are live-only for now).
pub fn mock_script_for(scenario_name: &str) -> Option<Vec<CompletionResponse>> {
    match scenario_name {
        "create-hello-file" => Some(vec![
            write_file(
                "hello.txt",
                "hello from liberado\n",
            ),
            report_ok("wrote hello.txt", &["hello.txt"]),
        ]),
        "edit-existing-readme" => Some(vec![
            write_file(
                "README.md",
                "# Project\n\nSome text.\n## Liberado\n",
            ),
            report_ok("appended Liberado heading", &["README.md"]),
        ]),
        "multi-file-feature" => Some(vec![
            write_file(
                "src/lib.rs",
                "pub fn add(a: i32, b: i32) -> i32 { a + b }\n",
            ),
            write_file(
                "tests/add_test.rs",
                "#[test]\nfn t() { assert_eq!(demo::add(2, 2), 4); }\n",
            ),
            report_ok(
                "added add + test",
                &["src/lib.rs", "tests/add_test.rs"],
            ),
        ]),
        "scoped-change-no-secrets" => Some(vec![
            write_file("notes/todo.md", "buy milk\n"),
            report_ok("wrote notes/todo.md only", &["notes/todo.md"]),
        ]),
        "ambiguous-no-op-should-fail" => Some(vec![report_failed(
            "no real work requested; honest failure",
        )]),
        // Stress (optional mock coverage for CI depth without live models)
        "rename-across-modules" => Some(vec![
            write_file(
                "src/lib.rs",
                "pub fn hello_world() -> &'static str { \"hi\" }\npub fn unused_helper() {}\n",
            ),
            write_file(
                "src/main.rs",
                "fn main() { println!(\"{}\", demo::hello_world()); }\n",
            ),
            report_ok(
                "renamed greet → hello_world",
                &["src/lib.rs", "src/main.rs"],
            ),
        ]),
        "repair-broken-unit-test" => Some(vec![
            write_file(
                "src/lib.rs",
                "pub fn double(x: i32) -> i32 { x + x }\n\n#[cfg(test)]\nmod tests {\n    use super::*;\n    #[test]\n    fn d() { assert_eq!(double(2), 4); }\n}\n",
            ),
            report_ok("fixed double off-by-one", &["src/lib.rs"]),
        ]),
        _ => None,
    }
}

fn write_file(path: &str, content: &str) -> CompletionResponse {
    CompletionResponse::tool_calls(vec![ToolInvocation::new(
        format!("w-{path}"),
        "write_file",
        json!({"path": path, "content": content}),
    )])
}

fn report_ok(summary: &str, artifacts: &[&str]) -> CompletionResponse {
    CompletionResponse::tool_calls(vec![ToolInvocation::new(
        "report-ok",
        liberado_executor::SUBMIT_REPORT_TOOL,
        json!({
            "outcome": "succeeded",
            "summary": summary,
            "artifacts": artifacts,
            "new_high_signal_facts": [],
            "follow_up": null
        }),
    )])
}

fn report_failed(summary: &str) -> CompletionResponse {
    CompletionResponse::tool_calls(vec![ToolInvocation::new(
        "report-fail",
        liberado_executor::SUBMIT_REPORT_TOOL,
        json!({
            "outcome": "failed",
            "summary": summary,
            "artifacts": [],
            "new_high_signal_facts": [],
            "follow_up": null
        }),
    )])
}

/// Scenarios in a tier that have mock scripts (ordered).
pub fn mockable_scenarios(tier: CoderTier) -> Vec<CoderScenario> {
    coder_scenarios_for(tier, None, None)
        .into_iter()
        .filter(|s| mock_script_for(s.name).is_some())
        .collect()
}

/// Run one scenario with its mock script through real Liberado scoring.
pub async fn score_mock_scenario(scenario: CoderScenario) -> CoderFitness {
    let script = mock_script_for(scenario.name)
        .unwrap_or_else(|| panic!("no mock script for {}", scenario.name));
    let provider = Arc::new(MockProvider::with_script("mock-curriculum", script));
    let budget = Budget::new(32);
    score_coder_candidate(
        DEFAULT_CODER_SYSTEM_PROMPT,
        &[provider],
        1,
        scenario.tier,
        Some(1),
        Some(&[scenario.name.to_string()]),
        10,
        &budget,
    )
    .await
}

/// Run all mockable scenarios up through `tier`; returns (name, passed) pairs.
pub async fn run_mock_curriculum(tier: CoderTier) -> Vec<(&'static str, bool)> {
    let mut out = Vec::new();
    for scenario in mockable_scenarios(tier) {
        let fitness = score_mock_scenario(scenario).await;
        let passed = fitness.accuracy >= 0.99
            && fitness.scenarios.first().map(|s| s.pass_rate() >= 0.99).unwrap_or(false);
        out.push((scenario.name, passed));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_curriculum_smoke_all_pass() {
        let results = run_mock_curriculum(CoderTier::Smoke).await;
        assert!(!results.is_empty(), "expected smoke mock scenarios");
        for (name, passed) in &results {
            assert!(passed, "smoke scenario {name} should pass under mock script");
        }
    }

    #[tokio::test]
    async fn mock_curriculum_core_all_pass() {
        // core includes smoke; all mockable through core must pass.
        let results = run_mock_curriculum(CoderTier::Core).await;
        assert!(
            results.len() >= 5,
            "expected smoke+core mock coverage, got {}",
            results.len()
        );
        for (name, passed) in &results {
            assert!(passed, "core curriculum scenario {name} should pass under mock");
        }
    }

    #[tokio::test]
    async fn mock_curriculum_stress_scripts_pass_when_present() {
        let results = run_mock_curriculum(CoderTier::Stress).await;
        // At least smoke+core; stress adds rename + repair when scripted.
        assert!(results.len() >= 5);
        for (name, passed) in &results {
            assert!(passed, "stress mock scenario {name} should pass");
        }
        assert!(
            results.iter().any(|(n, _)| *n == "rename-across-modules"),
            "expected rename stress script"
        );
    }

    #[test]
    fn every_smoke_and_core_has_mock_script() {
        for s in coder_scenarios_for(CoderTier::Core, None, None) {
            assert!(
                mock_script_for(s.name).is_some(),
                "missing mock script for curriculum scenario {}",
                s.name
            );
        }
    }
}
