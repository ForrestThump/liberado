//! Ad-hoc live reproduction for `docs/roadmap/multi-step-execution-reliability-finding.md`.
//!
//! Not part of the tuner proper — a throwaway diagnostic to see *why*, not just *whether*,
//! `multi-step-research` fails, across a few real models. Prints the full transcript signal the
//! aggregate tuner scoring throws away: every tool actually invoked, in order, and the model's own
//! final `Report` (outcome + summary) verbatim, so a hedge toward `PartiallySucceeded` or a genuine
//! missed call is visible directly instead of inferred from a pass/fail boolean.
//!
//! Run: `OPENROUTER_API_KEY=... cargo run -p liberado-heuristics-tuner --example debug_multistep`

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use liberado_executor::{Budget, Executor, Task, ToolRuntime};
use liberado_heuristics_tuner::tool_scenarios::tool_loop_scenarios;
use liberado_orchestrator::{DIRECT_INSTRUCTIONS, DIRECT_MAX_TURNS};
use liberado_provider::{Provider, ToolDef, ToolInvocation};
use liberado_provider_openai_compat::OpenAiCompatibleProvider;

struct ScriptedToolRuntime {
    tools: Vec<ToolDef>,
    canned: std::collections::HashMap<String, String>,
    invoked: Mutex<Vec<ToolInvocation>>,
}

impl ScriptedToolRuntime {
    fn new(tools: &'static [(&'static str, &'static str, &'static str)]) -> Self {
        let defs = tools
            .iter()
            .map(|(name, desc, _)| {
                ToolDef::new(*name, *desc, serde_json::json!({ "type": "object" }))
            })
            .collect();
        let canned = tools
            .iter()
            .map(|(name, _, result)| (name.to_string(), result.to_string()))
            .collect();
        Self {
            tools: defs,
            canned,
            invoked: Mutex::new(Vec::new()),
        }
    }

    fn invoked(&self) -> Vec<ToolInvocation> {
        self.invoked.lock().unwrap().clone()
    }
}

#[async_trait]
impl ToolRuntime for ScriptedToolRuntime {
    fn catalog(&self) -> Vec<ToolDef> {
        self.tools.clone()
    }

    async fn invoke(&self, call: &ToolInvocation) -> Result<String, String> {
        self.invoked.lock().unwrap().push(call.clone());
        self.canned
            .get(&call.name)
            .cloned()
            .ok_or_else(|| format!("no scripted result for tool '{}'", call.name))
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(
            "liberado_executor=debug,warn",
        ))
        .init();

    let api_key = std::env::var("OPENROUTER_API_KEY").expect("set OPENROUTER_API_KEY");

    let models = [
        "deepseek/deepseek-v4-flash",
        "google/gemini-3-flash-preview",
    ];
    const SAMPLES: usize = 3;

    let scenario = tool_loop_scenarios()
        .into_iter()
        .find(|s| s.name == "multi-step-research")
        .expect("multi-step-research scenario must exist");

    println!("=== goal: {} ===", scenario.goal);
    println!("=== prompt under test: DIRECT_INSTRUCTIONS ===\n{DIRECT_INSTRUCTIONS}\n");

    for model in models {
        println!("\n########## model: {model} ##########");
        let provider: Arc<dyn Provider> = Arc::new(OpenAiCompatibleProvider::new(
            api_key.clone(),
            model,
            OpenAiCompatibleProvider::OPENROUTER_BASE_URL,
        ));
        let executor = Executor::new(provider, Budget::new(DIRECT_MAX_TURNS));

        for sample in 1..=SAMPLES {
            let runtime = ScriptedToolRuntime::new(scenario.tools);
            let result = executor
                .execute(&runtime, Task::new(DIRECT_INSTRUCTIONS, scenario.goal))
                .await;

            let invoked = runtime.invoked();
            let called: Vec<&str> = invoked.iter().map(|c| c.name.as_str()).collect();
            let must_call_satisfied = scenario.expect.must_call.iter().all(|t| called.contains(t));

            println!("--- sample {sample} ---");
            println!("tools invoked (in order): {called:?}");
            for (i, inv) in invoked.iter().enumerate() {
                println!("  call {i}: {} args={}", inv.name, inv.arguments);
            }
            println!("must_call satisfied: {must_call_satisfied}");
            match result {
                Ok(report) => {
                    println!("outcome: {:?}", report.outcome);
                    println!(
                        "outcome_matched (expected {:?}): {}",
                        scenario.expect.expected_outcome,
                        report.outcome == scenario.expect.expected_outcome
                    );
                    println!("summary: {}", report.summary);
                    println!("artifacts: {:?}", report.artifacts);
                    println!("follow_up: {:?}", report.follow_up);
                }
                Err(e) => {
                    println!("EXECUTION ERROR: {e}");
                }
            }
        }
    }
}
