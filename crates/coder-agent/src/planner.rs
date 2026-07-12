//! Optional planner role: produce a short implementation plan before the worker acts.
//!
//! Skipped when the planner role has no prompt / prompt_path (config-skippable). Maker ≠ checker:
//! the planner does not own verifiers and does not mutate the workspace.

use chrono::Utc;
use liberado_coder_core::{CoderError, CoderEvent, CoderRunRequest};
use liberado_provider::{CompletionRequest, Message};
use serde_json::json;

use crate::CoderProviderFactory;
use crate::roles::{planner_enabled, role_instructions, truncate_chars};
use crate::trace::{self, EventLog};

const DEFAULT_PLANNER_SYSTEM: &str = "\
You are Liberado's coding planner. Given a task, produce a concise implementation plan. \
Do NOT write code. Do NOT invent acceptance tests the harness did not provide. \
Return ONLY JSON matching the schema.";

#[derive(Debug, Clone)]
pub struct PlanOutput {
    pub summary: String,
    pub steps: Vec<String>,
    pub likely_files: Vec<String>,
    pub risks: Vec<String>,
}

impl PlanOutput {
    /// Render plan as context text for the worker / repair goal.
    pub fn as_context_block(&self) -> String {
        let mut out =
            String::from("## Planner plan (do not invent new gates; follow frozen verifiers)\n");
        if !self.summary.trim().is_empty() {
            out.push_str("Summary: ");
            out.push_str(self.summary.trim());
            out.push('\n');
        }
        if !self.steps.is_empty() {
            out.push_str("Steps:\n");
            for (i, step) in self.steps.iter().enumerate() {
                out.push_str(&format!("{}. {}\n", i + 1, step));
            }
        }
        if !self.likely_files.is_empty() {
            out.push_str("Likely files:\n");
            for f in &self.likely_files {
                out.push_str("- ");
                out.push_str(f);
                out.push('\n');
            }
        }
        if !self.risks.is_empty() {
            out.push_str("Risks:\n");
            for r in &self.risks {
                out.push_str("- ");
                out.push_str(r);
                out.push('\n');
            }
        }
        out
    }
}

/// Run the planner when configured. Returns `None` when planner is disabled.
pub async fn run_planner(
    providers: &dyn CoderProviderFactory,
    request: &CoderRunRequest,
    events: &EventLog,
) -> Result<Option<PlanOutput>, CoderError> {
    if !planner_enabled(request) {
        return Ok(None);
    }

    let role = &request.config.planner;
    trace::push_event(
        events,
        CoderEvent::RoleStarted {
            role: "planner".to_string(),
            model: role.model.clone(),
            at: Utc::now(),
        },
    );

    let instructions = match role_instructions(role, "planner").await {
        Ok(text) => text,
        Err(_) => DEFAULT_PLANNER_SYSTEM.to_string(),
    };

    let mut user = format!("Task:\n{}\n", request.task.description);
    if let Some(context) = &request.task.context {
        user.push_str("\nContext:\n");
        user.push_str(context);
        user.push('\n');
    }
    if !request.task.success_criteria.is_empty() {
        user.push_str("\nSuccess criteria (prose):\n");
        for c in &request.task.success_criteria {
            user.push_str("- ");
            user.push_str(c);
            user.push('\n');
        }
    }
    if !request.config.verifiers.is_empty() {
        user.push_str(
            "\nFrozen verifier check ids (harness owns these; plan how to satisfy them):\n",
        );
        for v in &request.config.verifiers {
            user.push_str("- ");
            user.push_str(v.id());
            user.push_str(" (");
            user.push_str(v.kind());
            user.push_str(")\n");
        }
    }
    user.push_str("\nProduce the Plan JSON now.");

    let provider = providers.provider_for("planner", role)?;
    let mut completion =
        CompletionRequest::new(vec![Message::system(instructions), Message::user(user)]);
    if let Some(temperature) = role.temperature {
        completion = completion.with_temperature(temperature);
    } else {
        completion = completion.with_temperature(0.2);
    }
    if let Some(max_tokens) = role.max_tokens {
        completion = completion.with_max_tokens(max_tokens);
    } else {
        completion = completion.with_max_tokens(2048);
    }

    let schema = json!({
        "type": "object",
        "properties": {
            "summary": { "type": "string" },
            "steps": { "type": "array", "items": { "type": "string" } },
            "likely_files": { "type": "array", "items": { "type": "string" } },
            "risks": { "type": "array", "items": { "type": "string" } }
        },
        "required": ["summary", "steps"]
    });

    let response = provider
        .complete(completion.with_json_schema(schema))
        .await
        .map_err(|e| CoderError::Provider(format!("planner complete: {e}")))?;
    let content = response
        .content
        .as_deref()
        .ok_or_else(|| CoderError::Provider("planner returned empty content".into()))?;
    let plan = parse_plan(content)
        .map_err(|e| CoderError::Provider(format!("planner plan parse failed: {e}")))?;

    // Soft guard: empty plan is not fatal — worker continues without it.
    if plan.steps.is_empty() && plan.summary.trim().is_empty() {
        trace::push_event(
            events,
            CoderEvent::RoleFinished {
                role: "planner".to_string(),
                at: Utc::now(),
            },
        );
        return Ok(None);
    }

    let _ = truncate_chars(&plan.as_context_block(), 8_000);
    trace::push_event(
        events,
        CoderEvent::RoleFinished {
            role: "planner".to_string(),
            at: Utc::now(),
        },
    );
    Ok(Some(plan))
}

fn parse_plan(raw: &str) -> Result<PlanOutput, String> {
    let trimmed = raw.trim();
    let json_str = if let Some(start) = trimmed.find('{') {
        let end = trimmed
            .rfind('}')
            .ok_or("no closing brace in planner JSON")?;
        &trimmed[start..=end]
    } else {
        trimmed
    };
    let v: serde_json::Value =
        serde_json::from_str(json_str).map_err(|e| format!("{e}: {json_str}"))?;
    let string_list = |key: &str| -> Vec<String> {
        v.get(key)
            .and_then(|x| x.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|i| i.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    Ok(PlanOutput {
        summary: v
            .get("summary")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string(),
        steps: string_list("steps"),
        likely_files: string_list("likely_files"),
        risks: string_list("risks"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fenced_plan_json() {
        let raw = r#"```json
{"summary":"add hello","steps":["write hello.txt"],"likely_files":["hello.txt"],"risks":[]}
```"#;
        let plan = parse_plan(raw).unwrap();
        assert_eq!(plan.summary, "add hello");
        assert_eq!(plan.steps.len(), 1);
        assert!(plan.as_context_block().contains("hello.txt"));
    }
}
