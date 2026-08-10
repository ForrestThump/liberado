//! Role selection and prompt/goal assembly for the coding domain pack.
//!
//! Role *slots* (planner/worker/critic/repair) are a general goal-session idea; the coding pack
//! binds them to `CoderRoleConfig` and workspace-oriented goals. A future neutral session kernel
//! should own the graph; this module stays coding-facing.

use liberado_coder_core::{CoderError, CoderRoleConfig, CoderRunRequest};

pub fn worker_role_name(request: &CoderRunRequest) -> &'static str {
    if request.attempt > 0 && request.config.repair.is_some() {
        "repair"
    } else {
        "coder"
    }
}

pub fn worker_role_config(request: &CoderRunRequest) -> &CoderRoleConfig {
    if request.attempt > 0
        && let Some(repair) = &request.config.repair
    {
        return repair;
    }
    &request.config.coder
}

/// Instructions for a cold reviewer that has the diff and nothing else.
///
/// Cold is the point. A reviewer that has read the run's narration adopts it — it arrives already
/// persuaded, and a second opinion that agrees by construction is not a second opinion. This one
/// sees the change and the task, and is asked the question a human reviewer asks first.
///
/// The mutation question is here because it is the check this repo keeps failing. Two runs in a
/// row shipped tests that passed with the fix reverted; both were caught by hand, afterwards. It
/// is also a question answerable *from a diff*, which most correctness questions are not: you can
/// see whether an assertion could distinguish the new behaviour from the old one.
///
/// Lives beside the role machinery rather than at a call site so the reviewer being scored and
/// the reviewer that runs are the same text. A measurement against a copy of the prompt measures
/// the copy.
pub const COLD_DIFF_REVIEWER_PROMPT: &str = r#"You review a code change. You have the diff and the
task it was meant to accomplish. You did not see the work happen and you should not assume it was
done well or badly.

Judge these, in order:

1. Does the change do what the task asked?
2. For every test added or modified: what mutation of the production code would make it fail? If
   you cannot name one, the test does not cover the change and you must say so. A test that
   exercises a function the diff does not touch is the common case - check which code each test
   actually reaches.
3. Does anything here contradict a stated convention, or a comment elsewhere in the diff?

Do not ask for more tests, more docs, or style changes. Report defects, not preferences.

Respond with JSON only:
{"quality":"acceptable"} or {"quality":"needs_revision","issues":["...","..."]}"#;

pub fn critic_enabled(request: &CoderRunRequest) -> bool {
    request.config.critic.prompt.is_some() || request.config.critic.prompt_path.is_some()
}

/// Planner runs when configured with a prompt (or prompt_path). Empty = skip.
pub fn planner_enabled(request: &CoderRunRequest) -> bool {
    request.config.planner.prompt.is_some() || request.config.planner.prompt_path.is_some()
}

pub async fn role_instructions(
    role: &CoderRoleConfig,
    role_name: &str,
) -> Result<String, CoderError> {
    if let Some(prompt) = role.prompt.clone() {
        return Ok(prompt);
    }
    if let Some(path) = &role.prompt_path {
        return tokio::fs::read_to_string(path)
            .await
            .map_err(|e| CoderError::Setup(format!("read {role_name} prompt_path {path}: {e}")));
    }
    Err(CoderError::Setup(format!(
        "{role_name} role requires prompt or prompt_path"
    )))
}

pub fn coder_goal(request: &CoderRunRequest) -> String {
    let mut goal = format!("Task: {}", request.task.description);
    if let Some(context) = &request.task.context {
        goal.push_str("\n\nContext:\n");
        goal.push_str(context);
    }
    if !request.task.success_criteria.is_empty() {
        goal.push_str("\n\nSuccess criteria:\n");
        for criterion in &request.task.success_criteria {
            goal.push_str("- ");
            goal.push_str(criterion);
            goal.push('\n');
        }
    }
    // A strategist directive outranks everything below it. It only exists because several
    // attempts already failed the same way, so the tactical repair hints have demonstrably not
    // worked — burying it under them would reproduce the loop it exists to break. Rendered on
    // every attempt that carries one, never routed through `repair_focus_block`.
    if let Some(directive) = &request.strategist_directive {
        goal.push_str("\n\n## Structural directive (read this first)\n");
        goal.push_str(
            "Earlier attempts were refused repeatedly for the same reasons. A strategist reviewed \
             the goal and the rejection history and proposes ONE structural change. Apply it. It \
             does not relax the success criteria above — those still hold in full.\n\n",
        );
        goal.push_str(directive);
        goal.push('\n');
    }
    // Repair attempts: put failure-signature routing first so the model prioritizes it.
    if request.attempt > 0 {
        if let Some(focus) = crate::repair_feedback::repair_focus_block(&request.prior_feedback) {
            goal.push_str("\n\n");
            goal.push_str(&focus);
        }
    } else if !request.prior_feedback.is_empty() {
        goal.push_str("\n\nPrior feedback (from earlier attempts / guards / critic):\n");
        for feedback in &request.prior_feedback {
            goal.push_str("- ");
            goal.push_str(feedback);
            goal.push('\n');
        }
    }
    goal
}

pub fn truncate_chars(value: &str, max_chars: usize) -> String {
    let count = value.chars().count();
    if count <= max_chars {
        return value.to_string();
    }
    let mut out: String = value.chars().take(max_chars).collect();
    out.push_str("\n…[truncated]");
    out
}
