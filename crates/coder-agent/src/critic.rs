//! Model critic for the coding domain pack: reviews *evidence* (real git diff), not the worker's
//! self-report. Maker ≠ checker.

use chrono::Utc;
use liberado_coder_core::{CoderError, CoderEvent, CoderRunRequest, CriticVerdict};
use liberado_provider::{CompletionRequest, Message, complete_json};
use serde_json::json;

use crate::CoderProviderFactory;
use crate::roles::{role_instructions, truncate_chars};
use crate::trace::{self, EventLog};
use crate::workspace_diff;

pub async fn run_critic(
    providers: &dyn CoderProviderFactory,
    request: &CoderRunRequest,
    events: &EventLog,
) -> Result<Option<CriticVerdict>, CoderError> {
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

    // A critic is advisory after deterministic checks pass. A provider failure, including a
    // backend that rejects `json_schema` after the helper's plain-JSON retry is an abstention.
    let verdict: CriticVerdict = match complete_json(provider.as_ref(), completion, schema).await {
        Ok(verdict) => verdict,
        Err(e) => {
            tracing::warn!(error = %e, "critic structured completion failed; abstaining");
            return Ok(None);
        }
    };

    // A reviewer that fails to answer has not judged the change.
    //
    // This returned `Err` and destroyed two completed runs: the work was done, the deterministic
    // verifiers had passed, and the attempt was filed `Failed` because a provider handed back an
    // empty body. An empty or unparseable response is a fault in the *reviewer*, not a verdict on
    // the diff, and the deterministic gates — which since backlog 0.2 include the test suite — are
    // the authoritative bar. So the critic abstains and the run stands on them.
    //
    // Abstention is `None`, never `Acceptable`. Silently approving would fabricate a review that
    // nobody performed, which is worse than the bug being fixed: a discarded run is visibly wrong,
    // an invented approval is not.
    trace::push_event(
        events,
        CoderEvent::RoleFinished {
            role: "critic".to_string(),
            at: Utc::now(),
        },
    );
    Ok(Some(verdict))
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
        let v = parse_critic_verdict(r#"{"quality":"needs_revision","issues":["add more tests"]}"#)
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
