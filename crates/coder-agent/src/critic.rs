//! Model critic for the coding domain pack: reviews *evidence* (real git diff), not the worker's
//! self-report. Maker ≠ checker.

use chrono::Utc;
use liberado_coder_core::{CoderError, CoderEvent, CoderRunRequest, CriticVerdict};
use liberado_provider::{CompletionRequest, Message};
use serde_json::json;
use tokio::process::Command;

use crate::roles::{role_instructions, truncate_chars};
use crate::trace::{self, EventLog};
use crate::CoderProviderFactory;

pub async fn run_critic(
    providers: &dyn CoderProviderFactory,
    request: &CoderRunRequest,
    events: &EventLog,
) -> Result<CriticVerdict, CoderError> {
    trace::push_event(
        events,
        CoderEvent::RoleStarted {
            role: "critic".to_string(),
            model: request.config.critic.model.clone(),
            at: Utc::now(),
        },
    );

    let provider = providers.provider_for("critic", &request.config.critic)?;
    let instructions = role_instructions(&request.config.critic, "critic").await?;
    let diff = git_diff_for_critic(&request.workspace.root).await?;
    let mut user = format!(
        "Task:\n{}\n\nSuccess criteria:\n{}\n\nUnified git diff (against HEAD / worktree):\n```\n{}\n```\n\n\
         Respond with JSON only: {{\"quality\":\"acceptable\"}} or \
         {{\"quality\":\"needs_revision\",\"issues\":[\"...\"]}}.",
        request.task.description,
        if request.task.success_criteria.is_empty() {
            "(none listed)".to_string()
        } else {
            request
                .task
                .success_criteria
                .iter()
                .map(|c| format!("- {c}"))
                .collect::<Vec<_>>()
                .join("\n")
        },
        truncate_chars(&diff, 48_000),
    );
    if let Some(context) = &request.task.context {
        user.push_str("\n\nTask context:\n");
        user.push_str(context);
    }

    let mut completion = CompletionRequest::new(vec![
        Message::system(instructions),
        Message::user(user),
    ]);
    if let Some(temperature) = request.config.critic.temperature {
        completion = completion.with_temperature(temperature);
    }
    if let Some(max_tokens) = request.config.critic.max_tokens {
        completion = completion.with_max_tokens(max_tokens);
    }

    let schema = json!({
        "type": "object",
        "properties": {
            "quality": {
                "type": "string",
                "enum": ["acceptable", "needs_revision"]
            },
            "issues": {
                "type": "array",
                "items": { "type": "string" }
            }
        },
        "required": ["quality"]
    });

    let response = provider
        .complete(completion.with_json_schema(schema))
        .await
        .map_err(|e| CoderError::Provider(e.to_string()))?;
    let content = response
        .content
        .as_deref()
        .ok_or_else(|| CoderError::Provider("critic returned empty content".to_string()))?;
    let verdict = parse_critic_verdict(content)
        .map_err(|e| CoderError::Provider(format!("critic verdict parse failed: {e}")))?;

    trace::push_event(
        events,
        CoderEvent::RoleFinished {
            role: "critic".to_string(),
            at: Utc::now(),
        },
    );
    Ok(verdict)
}

pub fn parse_critic_verdict(raw: &str) -> Result<CriticVerdict, String> {
    let trimmed = raw.trim();
    let json_slice = extract_json_object(trimmed).unwrap_or(trimmed);
    serde_json::from_str(json_slice).map_err(|e| format!("{e}; body={trimmed}"))
}

fn extract_json_object(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end >= start {
        Some(&text[start..=end])
    } else {
        None
    }
}

async fn git_diff_for_critic(workspace_root: &str) -> Result<String, CoderError> {
    let staged = Command::new("git")
        .args(["diff", "HEAD"])
        .current_dir(workspace_root)
        .output()
        .await
        .map_err(|e| CoderError::Backend(format!("git diff: {e}")))?;
    if !staged.status.success() {
        return Err(CoderError::Backend(format!(
            "git diff exited {:?}: {}",
            staged.status.code(),
            String::from_utf8_lossy(&staged.stderr)
        )));
    }
    let mut diff = String::from_utf8_lossy(&staged.stdout).into_owned();
    let untracked = Command::new("git")
        .args(["ls-files", "--others", "--exclude-standard"])
        .current_dir(workspace_root)
        .output()
        .await
        .map_err(|e| CoderError::Backend(format!("git ls-files: {e}")))?;
    if untracked.status.success() {
        let names = String::from_utf8_lossy(&untracked.stdout);
        if !names.trim().is_empty() {
            if !diff.is_empty() {
                diff.push('\n');
            }
            diff.push_str("# untracked files\n");
            diff.push_str(&names);
        }
    }
    if diff.trim().is_empty() {
        diff = "(empty diff)".to_string();
    }
    Ok(diff)
}
