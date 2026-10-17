//! Automation Editor face tools: list schedules, or file one edit card.
//!
//! Create, update, and delete do not write `topology.toml`. They file a permission
//! proposal. Approve is the only path that calls the schedule writer.

use std::sync::Arc;

use async_trait::async_trait;
use liberado_executor::ToolRuntime;
use liberado_provider::{ToolDef, ToolInvocation};
use serde_json::{Value, json};

pub const LIST_SCHEDULES_TOOL_NAME: &str = "list_schedules";
pub const CREATE_SCHEDULE_TOOL_NAME: &str = "create_schedule";
pub const UPDATE_SCHEDULE_TOOL_NAME: &str = "update_schedule";
pub const DELETE_SCHEDULE_TOOL_NAME: &str = "delete_schedule";

const SCHEDULE_FIELDS: &str = "\
Optional fields, all omitted unless you mean to change them: enabled, cron_expr \
(7-field cron, seconds first, UTC), goal, pool, profile, deliver, max_turns, \
job, habit_text, task_limit, capture_path, git_remote, git_branch, \
git_user_name, git_user_email, run_on_start, direct. \
update_schedule is a field patch: only named keys change. \
new_name renames the row. \
To remove job, habit_text, or deliver, list that name in remove. \
An empty goal is refused when the row has a job.";

/// The three tools that file a card. `list_schedules` is not one of them.
pub fn is_schedule_write_tool(name: &str) -> bool {
    matches!(
        name,
        CREATE_SCHEDULE_TOOL_NAME | UPDATE_SCHEDULE_TOOL_NAME | DELETE_SCHEDULE_TOOL_NAME
    )
}

pub fn schedule_tool_defs() -> Vec<ToolDef> {
    vec![
        ToolDef::new(
            LIST_SCHEDULES_TOOL_NAME,
            "List the schedules stored in topology.toml. This reads the file. \
             It does not use the boot snapshot. No approval card. \
             Each row includes cron_expr, schedule_text (code-generated English; do not rewrite it), \
             and next_fires (the next 5 local times, each with weekday and UTC). \
             Judge timing only from next_fires and schedule_text. \
             cron_expr is UTC, seconds first, and is only for edits.",
            json!({"type": "object", "properties": {}}),
        ),
        ToolDef::new(
            CREATE_SCHEDULE_TOOL_NAME,
            format!(
                "File a card to append one [[schedules]] row. The file is not changed \
                 until the human taps Approve. {SCHEDULE_FIELDS} name is required."
            ),
            json!({
                "type": "object",
                "properties": { "name": {"type": "string"} },
                "required": ["name"]
            }),
        ),
        ToolDef::new(
            UPDATE_SCHEDULE_TOOL_NAME,
            format!(
                "File a card to patch the [[schedules]] row named `name`. \
                 Only keys you send are written. Every other field stays, \
                 including job, habit_text, and deliver. \
                 The file is not changed until the human taps Approve. {SCHEDULE_FIELDS} \
                 name is the row to patch."
            ),
            update_patch_schema(),
        ),
        ToolDef::new(
            DELETE_SCHEDULE_TOOL_NAME,
            "File a card to remove the [[schedules]] row named `name`. \
             The file is not changed until the human taps Approve. Deny writes nothing.",
            json!({
                "type": "object",
                "properties": { "name": {"type": "string"} },
                "required": ["name"]
            }),
        ),
    ]
}

/// Plain-English description of one schedule edit. This is the card text.
pub fn describe_schedule_edit(tool: &str, args: &Value) -> Result<String, String> {
    let name = required_name(tool, args)?;
    let verb = match tool {
        CREATE_SCHEDULE_TOOL_NAME => "Create",
        UPDATE_SCHEDULE_TOOL_NAME => "Update",
        DELETE_SCHEDULE_TOOL_NAME => "Delete",
        _ => return Err(format!("unknown schedule tool `{tool}`")),
    };
    if tool == DELETE_SCHEDULE_TOOL_NAME {
        return Ok(format!("Delete schedule '{name}'."));
    }
    Ok(format!(
        "{verb} schedule '{name}'. {}",
        field_sentence(args)
    ))
}

fn update_patch_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": true,
        "properties": {
            "name": {"type": "string"},
            "new_name": {"type": "string"},
            "enabled": {"type": "boolean"},
            "cron_expr": {"type": "string"},
            "goal": {"type": "string"},
            "pool": {"type": "string"},
            "profile": {"type": "string"},
            "deliver": {"type": "boolean"},
            "max_turns": {"type": "integer"},
            "job": {"type": "string"},
            "habit_text": {"type": "string"},
            "task_limit": {"type": "integer"},
            "capture_path": {"type": "string"},
            "git_remote": {"type": "string"},
            "git_branch": {"type": "string"},
            "git_user_name": {"type": "string"},
            "git_user_email": {"type": "string"},
            "run_on_start": {"type": "boolean"},
            "direct": {"type": "boolean"},
            "remove": {"type": "array", "items": {"type": "string"}}
        },
        "required": ["name"]
    })
}

fn required_name(tool: &str, args: &Value) -> Result<String, String> {
    let name = args
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if name.is_empty() {
        return Err(format!("{tool} requires a non-empty `name` string"));
    }
    Ok(name.to_string())
}

fn field_sentence(args: &Value) -> String {
    let Some(obj) = args.as_object() else {
        return "No other fields.".into();
    };
    let mut parts = Vec::new();
    for (key, value) in obj {
        if key == "name" {
            continue;
        }
        parts.push(format!("{key} = {value}"));
    }
    if parts.is_empty() {
        "No other fields.".into()
    } else {
        parts.join("; ")
    }
}

/// List and propose, implemented by [`crate::ChatSessions`].
#[async_trait]
pub trait ScheduleEditorOps: Send + Sync {
    async fn list_schedules(&self) -> Result<String, String>;

    /// Write a permission proposal and return the text the model sees now.
    /// Does not edit `topology.toml`.
    async fn propose_edit(
        &self,
        session_id: &str,
        tool: &str,
        args: Value,
        rationale: &str,
    ) -> Result<String, String>;
}

/// Face runtime whose catalog is only the four schedule tools.
pub struct EditorRuntime {
    session_id: String,
    ops: Arc<dyn ScheduleEditorOps>,
}

impl EditorRuntime {
    pub fn new(session_id: String, ops: Arc<dyn ScheduleEditorOps>) -> Self {
        Self { session_id, ops }
    }
}

#[async_trait]
impl ToolRuntime for EditorRuntime {
    fn catalog(&self) -> Vec<ToolDef> {
        schedule_tool_defs()
    }

    async fn invoke(&self, call: &ToolInvocation) -> Result<String, String> {
        if call.name == LIST_SCHEDULES_TOOL_NAME {
            return self.ops.list_schedules().await;
        }
        if !is_schedule_write_tool(&call.name) {
            return Err(format!("unknown tool `{}`", call.name));
        }
        let rationale = describe_schedule_edit(&call.name, &call.arguments)?;
        self.ops
            .propose_edit(
                &self.session_id,
                &call.name,
                call.arguments.clone(),
                &rationale,
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::face::FaceRuntime;
    use async_trait::async_trait;
    use std::sync::atomic::AtomicBool;

    struct EmptyOps;

    #[async_trait]
    impl ScheduleEditorOps for EmptyOps {
        async fn list_schedules(&self) -> Result<String, String> {
            Ok("[]".into())
        }

        async fn propose_edit(
            &self,
            _session_id: &str,
            _tool: &str,
            _args: Value,
            _rationale: &str,
        ) -> Result<String, String> {
            Ok("filed".into())
        }
    }

    struct NoExtras;

    #[async_trait]
    impl ToolRuntime for NoExtras {
        fn catalog(&self) -> Vec<ToolDef> {
            Vec::new()
        }

        async fn invoke(&self, _: &ToolInvocation) -> Result<String, String> {
            Err("no extras".into())
        }
    }

    #[test]
    fn list_schedules_tool_returns_code_computed_fire_times() {
        let list = schedule_tool_defs()
            .into_iter()
            .find(|tool| tool.name == LIST_SCHEDULES_TOOL_NAME)
            .expect("list tool");
        assert!(
            list.description.contains("next_fires"),
            "{}",
            list.description
        );
        assert!(
            list.description.contains("schedule_text"),
            "{}",
            list.description
        );
        assert!(list.description.contains("UTC"), "{}", list.description);
    }

    #[test]
    fn editor_catalog_is_exactly_the_four_schedule_tools() {
        let rt = EditorRuntime::new("sess".into(), Arc::new(EmptyOps));
        let names: Vec<_> = rt.catalog().into_iter().map(|tool| tool.name).collect();
        assert_eq!(
            names,
            vec![
                LIST_SCHEDULES_TOOL_NAME,
                CREATE_SCHEDULE_TOOL_NAME,
                UPDATE_SCHEDULE_TOOL_NAME,
                DELETE_SCHEDULE_TOOL_NAME,
            ]
        );
    }

    #[test]
    fn a_normal_face_catalog_does_not_include_schedule_tools() {
        let rt = FaceRuntime::new(
            None,
            Arc::new(NoExtras),
            None,
            Arc::new(AtomicBool::new(false)),
            None,
        );
        let names: Vec<_> = rt.catalog().into_iter().map(|tool| tool.name).collect();
        assert!(names.iter().all(|name| !is_schedule_write_tool(name)));
        assert!(!names.iter().any(|name| name == LIST_SCHEDULES_TOOL_NAME));
    }

    #[test]
    fn describe_names_the_edit_in_plain_english() {
        let text = describe_schedule_edit(
            CREATE_SCHEDULE_TOOL_NAME,
            &json!({"name": "morning", "cron_expr": "0 0 9 * * * *", "goal": "inbox"}),
        )
        .unwrap();
        assert!(text.contains("Create schedule 'morning'"), "{text}");
        assert!(text.contains("cron_expr"), "{text}");
        assert!(text.contains("inbox"), "{text}");
        let deleted =
            describe_schedule_edit(DELETE_SCHEDULE_TOOL_NAME, &json!({"name": "evening"})).unwrap();
        assert_eq!(deleted, "Delete schedule 'evening'.");
    }

    #[test]
    fn update_schedule_schema_is_a_patch_of_optional_fields() {
        let update = schedule_tool_defs()
            .into_iter()
            .find(|tool| tool.name == UPDATE_SCHEDULE_TOOL_NAME)
            .expect("update tool");
        assert!(
            update.description.contains("patch"),
            "{}",
            update.description
        );
        assert!(
            update.description.contains("only named keys")
                || update.description.contains("Only keys"),
            "{}",
            update.description
        );
        let required = update.parameters["required"].as_array().expect("required");
        assert_eq!(required, &vec![json!("name")]);
        assert!(update.parameters["properties"].get("cron_expr").is_some());
        assert!(update.parameters["properties"].get("remove").is_some());
        assert!(
            update.parameters["required"]
                .as_array()
                .unwrap()
                .iter()
                .all(|item| item != "cron_expr")
        );
    }

    #[test]
    fn describe_rejects_a_blank_name() {
        let err =
            describe_schedule_edit(UPDATE_SCHEDULE_TOOL_NAME, &json!({"name": "  "})).unwrap_err();
        assert!(err.contains("non-empty"), "{err}");
    }
}
