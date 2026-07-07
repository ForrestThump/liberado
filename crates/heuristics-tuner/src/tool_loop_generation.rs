//! The executor/subagent-layer analogs of [`crate::generation`]'s `cold_start`/`mutate`:
//! [`cold_start_executor`] and [`mutate_executor`], describing an executor's job (bounded tool-use
//! loop, finish via `submit_report`) instead of a dispatcher's. Split into its own module
//! (2026-07-07, `docs/roadmap/hygiene-audit-2026-07-04.md`'s Priority 2 recommendation) alongside
//! `crate::tool_loop_search`, for the same reason — dispatcher- and executor-tuning code no longer
//! interleaved in one flat file. `request_justification` stays in `crate::generation`: it's
//! genuinely shared, used by both layers' search loops.

use liberado_provider::{CompletionRequest, Message, Provider, ProviderResult, complete_json};

use crate::generation::{GenerationError, PromptOutput, schema};
use crate::search::Budget;
use crate::tool_loop_scoring::ToolLoopScoredScenario;

const META_SYSTEM_PROMPT_EXECUTOR: &str = "\
You design system prompts for AI agent tool-use loops. Return ONLY JSON of the form \
{\"prompt\": \"...\"} where the value is a complete, ready-to-use system prompt.";

const TASK_DESCRIPTION_EXECUTOR: &str = "\
Design a system prompt for an AI executor: a bounded, adaptive tool-use agent. Given a goal and a \
catalog of available tools, the executor must accomplish the goal using the tools provided, taking \
as few steps as clearly suffice, then call the `submit_report` tool with a concise, high-signal \
result (an outcome, a summary, and any artifacts produced). It must not ask the user anything — if \
it cannot proceed, it should honestly report why rather than fabricating success. It must never \
call a tool that isn't actually relevant to the goal, even when one is available in its catalog, and \
should prefer the lightest tool that gets the job done over a heavier one when both would work.";

/// The executor-layer analog of [`crate::generation::cold_start`] — same Monte Carlo restart
/// shape, describing an executor's job instead of a dispatcher's.
pub async fn cold_start_executor(
    meta_provider: &dyn Provider,
    budget: &Budget,
) -> Result<String, GenerationError> {
    if !budget.spend() {
        return Err(GenerationError::BudgetExhausted);
    }
    let request = CompletionRequest::new(vec![
        Message::system(META_SYSTEM_PROMPT_EXECUTOR),
        Message::user(format!(
            "{TASK_DESCRIPTION_EXECUTOR}\n\nWrite the system prompt now."
        )),
    ])
    .with_temperature(0.7);

    let output: ProviderResult<PromptOutput> =
        complete_json(meta_provider, request, schema()).await;
    Ok(output?.prompt)
}

/// The executor-layer analog of [`crate::generation::mutate`] — same targeted-fix shape, but the
/// failure context comes from [`ToolLoopScoredScenario`] (which tools were/weren't called, what
/// outcome resulted) instead of a dispatcher misroute.
pub async fn mutate_executor(
    meta_provider: &dyn Provider,
    parent_prompt: &str,
    failing: &[&ToolLoopScoredScenario],
    budget: &Budget,
) -> Result<String, GenerationError> {
    if !budget.spend() {
        return Err(GenerationError::BudgetExhausted);
    }

    let failure_breakdown = if failing.is_empty() {
        "(none — propose a refinement anyway; small wording changes can still improve robustness \
         on goals outside this scenario set)"
            .to_string()
    } else {
        failing
            .iter()
            .map(|s| {
                format!(
                    "- \"{}\": goal=\"{}\" must_call={:?} must_not_call={:?} expected_outcome={:?} — {} — {}",
                    s.name,
                    s.goal,
                    s.expect.must_call,
                    s.expect.must_not_call,
                    s.expect.expected_outcome,
                    s.trial_breakdown(),
                    s.note
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    let request = CompletionRequest::new(vec![
        Message::system(META_SYSTEM_PROMPT_EXECUTOR),
        Message::user(format!(
            "{TASK_DESCRIPTION_EXECUTOR}\n\nHere is the current system prompt:\n---\n{parent_prompt}\n---\n\n\
             These scenarios failed:\n{failure_breakdown}\n\n\
             Write an improved version that fixes these failures without regressing scenarios \
             that already pass."
        )),
    ])
    .with_temperature(0.5);

    let output: ProviderResult<PromptOutput> =
        complete_json(meta_provider, request, schema()).await;
    Ok(output?.prompt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_provider::{CompletionResponse, MockProvider};

    fn tool_loop_scenario(name: &'static str) -> ToolLoopScoredScenario {
        ToolLoopScoredScenario {
            name,
            goal: "add a task",
            note: "a destructive tool was available but irrelevant",
            expect: crate::tool_scenarios::ToolLoopExpect {
                must_call: &["tasks"],
                must_not_call: &["vault-delete-all"],
                expected_outcome: liberado_common::Outcome::Succeeded,
            },
            trials: vec![crate::tool_loop_scoring::ToolLoopTrial {
                model: "test-model".to_string(),
                outcome: crate::tool_loop_scoring::ToolLoopOutcome {
                    calls_matched: false,
                    unsafe_call: true,
                    outcome_matched: true,
                },
            }],
        }
    }

    #[tokio::test]
    async fn cold_start_executor_returns_the_parsed_prompt() {
        let mock = MockProvider::with_script(
            "mock",
            [CompletionResponse::text(
                r#"{"prompt": "be a careful executor"}"#,
            )],
        );
        let budget = Budget::new(10);
        let prompt = cold_start_executor(&mock, &budget).await.unwrap();
        assert_eq!(prompt, "be a careful executor");
    }

    #[tokio::test]
    async fn cold_start_executor_refuses_when_budget_is_exhausted() {
        let mock =
            MockProvider::with_script("mock", [CompletionResponse::text(r#"{"prompt": "x"}"#)]);
        let budget = Budget::new(0);
        let err = cold_start_executor(&mock, &budget).await.unwrap_err();
        assert!(matches!(err, GenerationError::BudgetExhausted));
    }

    #[tokio::test]
    async fn mutate_executor_includes_failure_context_and_returns_prompt() {
        let mock = MockProvider::with_script(
            "mock",
            [CompletionResponse::text(
                r#"{"prompt": "improved executor prompt"}"#,
            )],
        );
        let budget = Budget::new(10);
        let failing = tool_loop_scenario("avoid-irrelevant-destructive-tool");
        let refs = vec![&failing];
        let prompt = mutate_executor(&mock, "old executor prompt", &refs, &budget)
            .await
            .unwrap();
        assert_eq!(prompt, "improved executor prompt");

        let sent = mock.last_request().unwrap();
        let user_message = &sent.messages[1].content;
        assert!(user_message.contains("avoid-irrelevant-destructive-tool"));
        assert!(user_message.contains("old executor prompt"));
        assert!(user_message.contains("vault-delete-all"));
    }

    #[tokio::test]
    async fn mutate_executor_with_no_failures_still_asks_for_a_refinement() {
        let mock = MockProvider::with_script(
            "mock",
            [CompletionResponse::text(
                r#"{"prompt": "refined executor prompt"}"#,
            )],
        );
        let budget = Budget::new(10);
        let prompt = mutate_executor(&mock, "old prompt", &[], &budget)
            .await
            .unwrap();
        assert_eq!(prompt, "refined executor prompt");
    }
}
