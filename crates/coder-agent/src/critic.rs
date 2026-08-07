//! Model critic for the coding domain pack: reviews *evidence* (real git diff), not the worker's
//! self-report. Maker ≠ checker.

use chrono::Utc;
use liberado_coder_core::{CoderError, CoderEvent, CoderRunRequest, CriticVerdict};
use liberado_provider::{CompletionRequest, Message};
use serde_json::json;

use crate::CoderProviderFactory;
use crate::roles::{role_instructions, truncate_chars};
use crate::trace::{self, EventLog};
use crate::workspace_diff;

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
    let diff = workspace_diff(&request.workspace.root).await?;
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

    let mut completion =
        CompletionRequest::new(vec![Message::system(instructions), Message::user(user)]);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_critic_verdict_acceptable() {
        let v = parse_critic_verdict(r#"{"quality":"acceptable"}"#).unwrap();
        assert_eq!(v, CriticVerdict::Acceptable);
    }

    #[test]
    fn parse_critic_verdict_needs_revision() {
        let v = parse_critic_verdict(
            r#"{"quality":"needs_revision","issues":["add more tests"]}"#,
        )
        .unwrap();
        assert_eq!(
            v,
            CriticVerdict::NeedsRevision {
                issues: vec!["add more tests".into()]
            }
        );
    }

    #[test]
    fn parse_critic_verdict_fenced_json() {
        let v = parse_critic_verdict("```json\n{\"quality\":\"acceptable\"}\n```").unwrap();
        assert_eq!(v, CriticVerdict::Acceptable);
    }

    #[test]
    fn parse_critic_verdict_malformed() {
        let err = parse_critic_verdict("not json at all").unwrap_err();
        assert!(err.contains("body="));
    }

    #[test]
    fn extract_json_object_plain() {
        let result = extract_json_object(r#"{"key":"val"}"#);
        assert_eq!(result, Some(r#"{"key":"val"}"#));
    }

    #[test]
    fn extract_json_object_embedded() {
        let result = extract_json_object(r#"prefix{"key":"val"}suffix"#);
        assert_eq!(result, Some(r#"{"key":"val"}"#));
    }

    #[test]
    fn extract_json_object_no_braces() {
        assert_eq!(extract_json_object("no braces at all"), None);
    }

    #[test]
    fn extract_json_object_unbalanced() {
        assert_eq!(extract_json_object(r#"{"open only"#), None);
    }
}
