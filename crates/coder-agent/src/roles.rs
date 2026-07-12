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
    if request.attempt > 0 {
        if let Some(repair) = &request.config.repair {
            return repair;
        }
    }
    &request.config.coder
}

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
