//! # liberado-scratchpad
//!
//! A per-execution todo-list tool for `liberado-executor`'s report-mode loop — "external working
//! memory" (`docs/future-work/ideas/archive/doomloop_research.md`), one of the highest-ROI doom-loop mitigations,
//! implemented as core engine state rather than a standalone MCP.
//!
//! Deliberately **not** an MCP: it follows the same shape as `liberado-executor`'s
//! `SUBMIT_REPORT_TOOL` precedent — a tool the model calls exactly like any other, but with no
//! server behind it. The executor recognizes [`SCRATCHPAD_TOOL`] directly in its own dispatch loop
//! and calls [`Scratchpad::apply`] in-process. This crate owns everything mode-agnostic (the data
//! shape, the tool schema, the mutation logic); `liberado-executor` owns only the wiring —
//! constructing one [`Scratchpad`] per execution, pushing [`Scratchpad::tool_def`] into the tool
//! catalog, and routing matching tool calls to [`Scratchpad::apply`] instead of a real
//! [`liberado_provider::ToolDef`]-backed runtime. No async, no I/O, no dependency on
//! `liberado-executor` — this crate is pure data and logic, reusable from any future call site
//! (e.g. conversational mode) with zero duplicated code.

use liberado_provider::ToolDef;
use serde::{Deserialize, Serialize};

/// Name of the synthetic scratchpad tool the engine injects in report mode. A real
/// [`liberado_executor::ToolRuntime`](../liberado_executor/trait.ToolRuntime.html) must not expose
/// a tool with this name (it would be shadowed by the engine's own interception).
pub const SCRATCHPAD_TOOL: &str = "scratchpad_write";

/// One todo item's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Todo,
    InProgress,
    Done,
}

/// One entry in the scratchpad. Named `TodoItem`, not `Task` — `liberado_executor::Task` already
/// names the *execution* task (goal/instructions/seed_calls); reusing "Task" here would be a real
/// collision risk if the two were ever imported together.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TodoItem {
    pub content: String,
    pub status: TodoStatus,
}

/// The arguments a `scratchpad_write` call carries — a full replacement list, never a partial
/// patch (matching the real `TodoWrite` tool's own semantics: simpler for the model to reason
/// about than indexed patch operations, and a malformed list just fails cleanly and keeps the
/// prior state, unlike a malformed patch). Private: the executor never names this type, only
/// [`Scratchpad::apply`]'s `&serde_json::Value` boundary.
#[derive(Debug, Deserialize)]
struct ScratchpadArgs {
    items: Vec<TodoItem>,
}

/// One execution's todo list. Constructed fresh per execution and dropped when it ends — there is
/// no persistence and none is wanted (see the module docs: this is working memory for *this* run).
#[derive(Debug, Default)]
pub struct Scratchpad {
    items: Vec<TodoItem>,
}

impl Scratchpad {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn items(&self) -> &[TodoItem] {
        &self.items
    }

    /// The tool definition to push into the catalog in report mode.
    pub fn tool_def() -> ToolDef {
        ToolDef::new(
            SCRATCHPAD_TOOL,
            "Write your complete todo list, replacing the previous state. Use this to track \
             multi-step progress: what you've done, what you're doing, and what remains. Send \
             the FULL list every call — items not listed are removed. Mark exactly one item \
             `in_progress` (the step you are currently executing). Whenever possible, bundle \
             this call alongside another tool call in the same turn rather than calling it \
             alone, so you don't spend a turn just on bookkeeping.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "items": {
                        "type": "array",
                        "description": "Your complete todo list, replacing any previous state.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "content": {
                                    "type": "string",
                                    "description": "What this step does, one line."
                                },
                                "status": {
                                    "type": "string",
                                    "enum": ["todo", "in_progress", "done"],
                                    "description": "todo: not started. in_progress: current step \
                                        (use for exactly one item). done: finished."
                                }
                            },
                            "required": ["content", "status"]
                        }
                    }
                },
                "required": ["items"]
            }),
        )
    }

    /// Replace the current list with `args`'s `items`, and describe the result for the model.
    /// Malformed args leave the prior state untouched and return a descriptive error string
    /// rather than panicking — this is fed straight back as the tool's result, same as any real
    /// tool's `Err` path.
    pub fn apply(&mut self, args: &serde_json::Value) -> String {
        let parsed: ScratchpadArgs = match serde_json::from_value(args.clone()) {
            Ok(parsed) => parsed,
            Err(e) => {
                return format!(
                    "Scratchpad update failed: {e}. Send an `items` array of {{content, status}} objects."
                );
            }
        };

        self.items = parsed.items;

        if self.items.is_empty() {
            return "Scratchpad cleared.".to_string();
        }

        let done = self.count(TodoStatus::Done);
        let in_progress = self.count(TodoStatus::InProgress);
        let todo = self.count(TodoStatus::Todo);
        format!("Scratchpad updated: {done} done, {in_progress} in_progress, {todo} todo.")
    }

    fn count(&self, status: TodoStatus) -> usize {
        self.items.iter().filter(|i| i.status == status).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(content: &str, status: TodoStatus) -> TodoItem {
        TodoItem {
            content: content.to_string(),
            status,
        }
    }

    #[test]
    fn new_scratchpad_is_empty() {
        assert!(Scratchpad::new().items().is_empty());
    }

    #[test]
    fn full_list_replace() {
        let mut pad = Scratchpad::new();
        pad.apply(&serde_json::json!({
            "items": [{"content": "a", "status": "todo"}]
        }));
        pad.apply(&serde_json::json!({
            "items": [{"content": "b", "status": "in_progress"}, {"content": "c", "status": "done"}]
        }));
        assert_eq!(
            pad.items(),
            &[
                item("b", TodoStatus::InProgress),
                item("c", TodoStatus::Done),
            ]
        );
    }

    #[test]
    fn apply_returns_status_counts() {
        let mut pad = Scratchpad::new();
        let result = pad.apply(&serde_json::json!({
            "items": [
                {"content": "a", "status": "done"},
                {"content": "b", "status": "done"},
                {"content": "c", "status": "in_progress"},
                {"content": "d", "status": "todo"},
            ]
        }));
        assert_eq!(result, "Scratchpad updated: 2 done, 1 in_progress, 1 todo.");
    }

    #[test]
    fn empty_list_clears_scratchpad() {
        let mut pad = Scratchpad::new();
        pad.apply(&serde_json::json!({"items": [{"content": "a", "status": "todo"}]}));
        let result = pad.apply(&serde_json::json!({"items": []}));
        assert!(pad.items().is_empty());
        assert_eq!(result, "Scratchpad cleared.");
    }

    #[test]
    fn malformed_args_returns_error_string_and_keeps_prior_state() {
        let mut pad = Scratchpad::new();
        pad.apply(&serde_json::json!({"items": [{"content": "a", "status": "todo"}]}));
        let result = pad.apply(&serde_json::json!({"wrong_field": 1}));
        assert!(result.starts_with("Scratchpad update failed:"));
        // Prior state is untouched by a malformed call.
        assert_eq!(pad.items(), &[item("a", TodoStatus::Todo)]);
    }

    #[test]
    fn tool_def_name_matches_constant() {
        assert_eq!(Scratchpad::tool_def().name, SCRATCHPAD_TOOL);
    }

    #[test]
    fn two_in_progress_items_are_accepted_not_enforced() {
        let mut pad = Scratchpad::new();
        pad.apply(&serde_json::json!({
            "items": [
                {"content": "a", "status": "in_progress"},
                {"content": "b", "status": "in_progress"},
            ]
        }));
        assert_eq!(pad.items().len(), 2);
    }
}
