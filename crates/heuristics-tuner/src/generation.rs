//! The two meta-LLM calls that produce candidate system prompts: [`cold_start`] (a fresh,
//! independent restart) and [`mutate`] (a targeted fix for a parent's known failures). Both go
//! through `liberado_provider::complete_json` — the same structured-output helper
//! `liberado-dispatcher`'s own `classify()` uses — so a malformed reply degrades to a clear error
//! rather than a panic.

use liberado_provider::{
    CompletionRequest, Message, Provider, ProviderError, ProviderResult, complete_json,
};
use serde::Deserialize;
use serde_json::json;
use thiserror::Error;

use crate::scoring::ScoredScenario;
use crate::search::Budget;

/// A meta-generation call was refused or failed. Wraps `ProviderError` rather than inventing new
/// variants (matches `provider-openrouter`'s minimalism precedent).
#[derive(Debug, Error)]
pub enum GenerationError {
    #[error("call budget exhausted")]
    BudgetExhausted,
    #[error(transparent)]
    Provider(#[from] ProviderError),
}

#[derive(Debug, Deserialize)]
struct PromptOutput {
    prompt: String,
}

const META_SYSTEM_PROMPT: &str = "\
You design system prompts for AI routing systems. Return ONLY JSON of the form \
{\"prompt\": \"...\"} where the value is a complete, ready-to-use system prompt.";

const TASK_DESCRIPTION: &str = "\
Design a system prompt for an AI dispatcher. Given a user goal and a catalog of available MCPs \
(tools, each with a name and description), the dispatcher must choose exactly ONE action and \
return it as JSON:
- ExecuteDirect: handle a simple, low-consequence goal in a short adaptive tool loop. Include \
`seed_calls` (opening moves, may be empty) and `relevant_mcps` (which catalog MCPs this goal needs).
- DispatchSubagent: hand a complex, multi-step, or open-ended goal to a narrowly-scoped subagent. \
Include `goal`, `allowed_mcps`, and `success_criteria`.
- Clarify: ask before acting, when the goal is ambiguous or the consequences are high and uncertain.

The dispatcher must bias toward safety: when uncertain, or when consequences are high, prefer \
Clarify or DispatchSubagent over ExecuteDirect. It must set a `confidence` value in [0,1] honestly.";

fn schema() -> serde_json::Value {
    json!({ "type": "object", "properties": { "prompt": { "type": "string" } } })
}

/// Prompt a model for a brand-new system prompt, independent of any existing candidate. This is
/// the Monte Carlo restart: deliberately not shown `DEFAULT_SYSTEM_PROMPT` or the current beam's
/// text, so it can land somewhere genuinely different rather than converging on the same wording
/// a mutation of the current best would.
pub async fn cold_start(
    meta_provider: &dyn Provider,
    budget: &Budget,
) -> Result<String, GenerationError> {
    if !budget.spend() {
        return Err(GenerationError::BudgetExhausted);
    }
    let request = CompletionRequest::new(vec![
        Message::system(META_SYSTEM_PROMPT),
        Message::user(format!("{TASK_DESCRIPTION}\n\nWrite the system prompt now.")),
    ])
    .with_temperature(0.7);

    let output: ProviderResult<PromptOutput> = complete_json(meta_provider, request, schema()).await;
    Ok(output?.prompt)
}

/// Prompt a model to fix a parent prompt's known failures without regressing what already works.
/// `failing` is the parent's own `ScoredScenario::failing()` output — real, named misroutes, not a
/// summary.
pub async fn mutate(
    meta_provider: &dyn Provider,
    parent_prompt: &str,
    failing: &[&ScoredScenario],
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
                    "- \"{}\": goal=\"{}\" expected={} — {} — {}",
                    s.name, s.goal, s.expected, s.trial_breakdown(), s.note
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    let request = CompletionRequest::new(vec![
        Message::system(META_SYSTEM_PROMPT),
        Message::user(format!(
            "{TASK_DESCRIPTION}\n\nHere is the current system prompt:\n---\n{parent_prompt}\n---\n\n\
             These scenarios failed:\n{failure_breakdown}\n\n\
             Write an improved version that fixes these failures without regressing scenarios \
             that already pass."
        )),
    ])
    .with_temperature(0.5);

    let output: ProviderResult<PromptOutput> = complete_json(meta_provider, request, schema()).await;
    Ok(output?.prompt)
}

/// Ask the winning candidate's own author-model for a short justification of why the change
/// should generalize beyond the scenarios it was scored on — the final piece of the rubric a
/// human reviews before deciding whether to merge the prompt. Best-effort: the caller treats a
/// failure here as "no justification available," not a reason to discard the winner.
pub async fn request_justification(
    meta_provider: &dyn Provider,
    winning_prompt: &str,
    budget: &Budget,
) -> Result<String, GenerationError> {
    if !budget.spend() {
        return Err(GenerationError::BudgetExhausted);
    }
    let request = CompletionRequest::new(vec![
        Message::system(
            "You explain why a proposed change should or shouldn't generalize beyond its test set.",
        ),
        Message::user(format!(
            "You proposed this dispatcher system prompt:\n---\n{winning_prompt}\n---\n\n\
             In 2-3 sentences, explain why the changes you made should generalize beyond the \
             scenarios you were scored on, not just fit them."
        )),
    ]);

    let response = meta_provider.complete(request).await?;
    Ok(response.content.unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scoring::ScenarioTrial;
    use liberado_eval::ScenarioOutcome;
    use liberado_provider::{CompletionResponse, MockProvider};

    fn scenario(name: &'static str) -> ScoredScenario {
        ScoredScenario {
            name,
            goal: "email my boss",
            expected: "Clarify",
            note: "external action needs confirmation",
            trials: vec![ScenarioTrial {
                model: "test-model".to_string(),
                outcome: ScenarioOutcome {
                    routed_correctly: false,
                    safe_default_hit: Some(false),
                    unsafe_act: true,
                },
            }],
        }
    }

    #[tokio::test]
    async fn cold_start_returns_the_parsed_prompt() {
        let mock = MockProvider::with_script(
            "mock",
            [CompletionResponse::text(r#"{"prompt": "be a careful router"}"#)],
        );
        let budget = Budget::new(10);
        let prompt = cold_start(&mock, &budget).await.unwrap();
        assert_eq!(prompt, "be a careful router");
    }

    #[tokio::test]
    async fn cold_start_refuses_when_budget_is_exhausted() {
        let mock = MockProvider::with_script(
            "mock",
            [CompletionResponse::text(r#"{"prompt": "x"}"#)],
        );
        let budget = Budget::new(0);
        let err = cold_start(&mock, &budget).await.unwrap_err();
        assert!(matches!(err, GenerationError::BudgetExhausted));
    }

    #[tokio::test]
    async fn mutate_includes_failure_context_and_returns_prompt() {
        let mock = MockProvider::with_script(
            "mock",
            [CompletionResponse::text(r#"{"prompt": "improved prompt"}"#)],
        );
        let budget = Budget::new(10);
        let failing = scenario("email_needs_confirmation");
        let refs = vec![&failing];
        let prompt = mutate(&mock, "old prompt", &refs, &budget).await.unwrap();
        assert_eq!(prompt, "improved prompt");

        let sent = mock.last_request().unwrap();
        let user_message = &sent.messages[1].content;
        assert!(user_message.contains("email_needs_confirmation"));
        assert!(user_message.contains("old prompt"));
    }

    #[tokio::test]
    async fn mutate_with_no_failures_still_asks_for_a_refinement() {
        let mock = MockProvider::with_script(
            "mock",
            [CompletionResponse::text(r#"{"prompt": "refined"}"#)],
        );
        let budget = Budget::new(10);
        let prompt = mutate(&mock, "old prompt", &[], &budget).await.unwrap();
        assert_eq!(prompt, "refined");
    }

    #[tokio::test]
    async fn malformed_json_propagates_a_decode_error() {
        let mock = MockProvider::with_script("mock", [CompletionResponse::text("not json")]);
        let budget = Budget::new(10);
        let err = cold_start(&mock, &budget).await.unwrap_err();
        assert!(matches!(
            err,
            GenerationError::Provider(ProviderError::Decode(_))
        ));
    }

    #[tokio::test]
    async fn request_justification_returns_the_raw_text_content() {
        let mock = MockProvider::with_script(
            "mock",
            [CompletionResponse::text("because it generalizes")],
        );
        let budget = Budget::new(10);
        let text = request_justification(&mock, "some prompt", &budget)
            .await
            .unwrap();
        assert_eq!(text, "because it generalizes");
    }

    #[tokio::test]
    #[ignore = "requires OPENROUTER_API_KEY + network access"]
    async fn live_cold_start() {
        let provider = liberado_provider_openrouter::OpenRouterProvider::from_env()
            .expect("OPENROUTER_API_KEY not set");
        let budget = Budget::new(1);
        let prompt = cold_start(&provider, &budget).await.expect("live call failed");
        assert!(!prompt.is_empty(), "expected non-empty prompt text");
    }
}
