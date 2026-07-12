//! Meta-LLM cold-start / mutate for the **coder role** system prompt.

use liberado_provider::{CompletionRequest, Message, Provider, ProviderResult, complete_json};

use crate::coder_scoring::CoderScoredScenario;
use crate::generation::{GenerationError, PromptOutput, schema};
use crate::search::Budget;

const META_SYSTEM: &str = "\
You design system prompts for autonomous coding agents that edit real repositories with discrete \
tools (read/search/write/edit/patch/git/validate/report). Return ONLY JSON of the form \
{\"prompt\": \"...\"} where the value is a complete, ready-to-use system prompt.";

const TASK: &str = "\
Design a system prompt for Liberado's coding worker. The agent runs in a bounded tool loop over a \
git workspace. It must make real file edits (not claim success without a diff), prefer precise \
edits over rewrites, avoid unrelated files, never commit/push/open PRs, and call submit_report with \
an honest outcome. It must not thrash on read-only exploration. False success is a critical failure.";

pub async fn cold_start_coder(
    meta_provider: &dyn Provider,
    budget: &Budget,
) -> Result<String, GenerationError> {
    if !budget.spend() {
        return Err(GenerationError::BudgetExhausted);
    }
    let request = CompletionRequest::new(vec![
        Message::system(META_SYSTEM),
        Message::user(format!("{TASK}\n\nWrite the system prompt now.")),
    ])
    .with_temperature(0.7);

    let output: ProviderResult<PromptOutput> =
        complete_json(meta_provider, request, schema()).await;
    Ok(output?.prompt)
}

pub async fn mutate_coder(
    meta_provider: &dyn Provider,
    parent_prompt: &str,
    failing: &[&CoderScoredScenario],
    budget: &Budget,
) -> Result<String, GenerationError> {
    if !budget.spend() {
        return Err(GenerationError::BudgetExhausted);
    }

    let failure_breakdown = if failing.is_empty() {
        "(none — propose a refinement anyway; small wording can still improve robustness)"
            .to_string()
    } else {
        failing
            .iter()
            .map(|s| {
                format!(
                    "- \"{}\": task=\"{}\" must_change={:?} must_not_change={:?} \
                     expected_outcome={:?} — {} — {}",
                    s.name,
                    s.task,
                    s.expect.must_change,
                    s.expect.must_not_change,
                    s.expect.expected_outcome,
                    s.trial_breakdown(),
                    s.note
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    let request = CompletionRequest::new(vec![
        Message::system(META_SYSTEM),
        Message::user(format!(
            "{TASK}\n\nCurrent system prompt:\n```\n{parent_prompt}\n```\n\n\
             Failures / weak scenarios on the coding eval set:\n{failure_breakdown}\n\n\
             Produce an improved complete system prompt. Keep it concise and actionable."
        )),
    ])
    .with_temperature(0.5);

    let output: ProviderResult<PromptOutput> =
        complete_json(meta_provider, request, schema()).await;
    Ok(output?.prompt)
}
