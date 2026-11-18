//! `tasks` — Obsidian-tasks vertical for TurboVault, as a compiled-in plugin module.
//!
//! This module extracts the historical task MCP tools (originally written on the
//! monolithic `tools.rs` against internal managers) and rebuilds them on the
//! stable plugin boundary. All vault access goes through the curated
//! [`VaultApi`]; writes are compare-and-swap only (`Match(version)`), so the
//! module can never blind-overwrite a note.
//!
//! Local tool names are unprefixed here; the host advertises them as
//! `tasks_<name>` (e.g. `tasks_list`, `tasks_complete`).
//!
//! # Tool surface (v1)
//!
//! | local name | kind | notes |
//! |---|---|---|
//! | `create`   | write | create a task line in a note (global-filter tag applied automatically) |
//! | `list`     | read  | all/open/done/cancelled, optional tag filter. Open is todo and in progress |
//! | `overdue`  | read  | open tasks past `due_date`, oldest first |
//! | `tags`     | read  | distinct inline task tags across the vault |
//! | `complete` | write | flip the checkbox + stamp `✅ <done>`; spawn the next occurrence of a recurring task (CAS) |
//! | `update`   | write | edit arbitrary fields and re-render the line (CAS) |
//! | `delete`   | write | remove the task line (CAS) |
//! | `config`   | read  | show the settings the module tuned itself to |
//!
//! ## Self-tuning
//!
//! The module renders edits (`update`) in the dialect the user's Obsidian Tasks
//! plugin uses — emoji or Dataview — and honors that plugin's global filter. It
//! learns both by reading `.obsidian/plugins/obsidian-tasks-plugin/data.json`
//! through the curated [`VaultApi::read_config`], falling back to a content
//! heuristic when the settings file is unavailable. See [`config`] and
//! [`render`].
//!
//! Writes read → parse (via core's `parse_tasks`) → edit → render
//! ([`render::to_markdown_line`], the write half owned here) → compare-and-swap.

mod config;
mod recurrence;
mod render;

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use async_trait::async_trait;
use jiff::civil::Date;
use serde_json::{Value, json};
use tokio::sync::OnceCell;
use turbomcp_types::ToolInputSchema;
use turbovault_core::{TaskItem, TaskPriority, TaskStatus};
use turbovault_plugin_api::{
    Plugin, PluginContext, PluginDescriptor, PluginError, PluginProvider, PluginRequestContext,
    PluginResult, Tool, ToolResult, VaultApi, WriteNoteRequest, WritePrecondition, WriteProvenance,
};

/// The task renderer and its dialect enum are part of this crate's public API:
/// the write half of the task round trip (its read half is core's
/// `parse_tasks`). `to_markdown_line` is also the seam a future recurrence-spawn
/// on `complete` will build on.
pub use config::{ConfigSource, TaskFormat, TasksConfig};
pub use render::to_markdown_line;

/// Compiled-in factory for the `tasks` module.
pub struct TasksPlugin;

impl Plugin for TasksPlugin {
    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor {
            id: "tasks".to_string(),
            name: "Tasks".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            description: "Obsidian-tasks queries and edits over the active vault".to_string(),
        }
    }

    fn build(&self, context: PluginContext) -> PluginResult<Arc<dyn PluginProvider>> {
        Ok(Arc::new(TasksProvider {
            vault: context.vault,
            config: OnceCell::new(),
        }))
    }
}

/// Runtime provider holding the curated vault facade.
pub struct TasksProvider {
    vault: VaultApi,
    /// Tasks-plugin settings, resolved once on first use. `Plugin::build` is
    /// synchronous and detection reads `.obsidian` via the async `VaultApi`, so
    /// it cannot run at construction time.
    config: OnceCell<TasksConfig>,
}

#[async_trait]
impl PluginProvider for TasksProvider {
    fn tools(&self) -> Vec<Tool> {
        vec![
            tool(
                "create",
                "Create a new task line in a note. Appends after existing tasks (or end of note if none). The configured global filter tag (e.g. #task) is added to the task's tags automatically — you never need to pass it yourself.",
                json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Vault-relative note path to write the task into" },
                        "content": { "type": "string", "description": "The task description text (just the prose — no checkbox, priority, dates, or tags; those come from the other parameters)" },
                        "priority": { "type": "string", "description": "highest | high | medium | low | lowest | normal" },
                        "due": { "type": "string", "description": "Due date, YYYY-MM-DD" },
                        "scheduled": { "type": "string", "description": "Scheduled date, YYYY-MM-DD" },
                        "start": { "type": "string", "description": "Start date, YYYY-MM-DD" },
                        "recurrence": { "type": "string", "description": "Recurrence rule, e.g. 'every week' or 'every 2 days'" },
                        "tags": { "type": "array", "items": { "type": "string" }, "description": "Additional inline tags (leading # optional). The global filter tag is added automatically — do not include it here." },
                        "insert_after_line": { "type": "integer", "minimum": 1, "description": "Insert the task after this 1-based line number (default: after the last existing task in the note, or end of note if there are none)" },
                        "expected_version": { "type": "string", "description": "Optimistic-concurrency token from a prior read; rejected if the note has changed" }
                    },
                    "required": ["path", "content"]
                }),
            ),
            tool(
                "list",
                "List tasks across the active vault, optionally filtered by status and tags",
                json!({
                    "type": "object",
                    "properties": {
                        "status": {
                            "type": "string",
                            "description": "all | pending | open | todo | incomplete | completed | done | cancelled (default all). Open filters are todo and in progress; they exclude cancelled.",
                            "enum": ["all", "pending", "open", "todo", "incomplete", "completed", "done", "cancelled"]
                        },
                        "tags": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Only tasks bearing all of these inline tags (leading # optional)"
                        }
                    }
                }),
            ),
            tool(
                "overdue",
                "List open tasks (todo or in progress) whose due date is before today (or a given date), oldest first. Cancelled tasks are excluded.",
                json!({
                    "type": "object",
                    "properties": {
                        "as_of": {
                            "type": "string",
                            "description": "YYYY-MM-DD; overdue is measured against this date (default today)"
                        }
                    }
                }),
            ),
            tool(
                "tags",
                "Return the deduplicated, sorted set of inline #tags used on task items",
                json!({ "type": "object", "properties": {} }),
            ),
            tool(
                "complete",
                "Mark a task complete: flip its checkbox to [x] and stamp today's done date",
                json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Vault-relative note path (from list)" },
                        "line": { "type": "integer", "minimum": 1, "description": "1-based line of the task (from list)" },
                        "expected_version": { "type": "string", "description": "Optimistic-concurrency token from a prior read; rejected if the note has changed" },
                        "done_date": { "type": "string", "description": "YYYY-MM-DD to stamp instead of today" }
                    },
                    "required": ["path", "line"]
                }),
            ),
            tool(
                "update",
                "Edit a task's fields and rewrite the line in the vault's Tasks dialect (emoji or dataview). Absent fields are left unchanged; pass null or \"\" to clear an optional field.",
                json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Vault-relative note path (from list)" },
                        "line": { "type": "integer", "minimum": 1, "description": "1-based line of the task (from list)" },
                        "expected_version": { "type": "string", "description": "Optimistic-concurrency token from a prior read; rejected if the note has changed" },
                        "content": { "type": "string", "description": "Replace the task description text" },
                        "completed": { "type": "boolean", "description": "Set the checkbox state" },
                        "priority": { "type": "string", "description": "highest | high | medium | low | lowest | normal" },
                        "due": { "type": "string", "description": "YYYY-MM-DD, or null/\"\" to clear" },
                        "scheduled": { "type": "string", "description": "YYYY-MM-DD, or null/\"\" to clear" },
                        "start": { "type": "string", "description": "YYYY-MM-DD, or null/\"\" to clear" },
                        "created": { "type": "string", "description": "YYYY-MM-DD, or null/\"\" to clear" },
                        "done": { "type": "string", "description": "YYYY-MM-DD, or null/\"\" to clear" },
                        "cancelled": { "type": "string", "description": "YYYY-MM-DD, or null/\"\" to clear" },
                        "recurrence": { "type": "string", "description": "Recurrence rule, e.g. 'every week', or null/\"\" to clear" },
                        "tags": { "type": "array", "items": { "type": "string" }, "description": "Replace the task's inline tags (leading # optional)" },
                        "add_tags": { "type": "array", "items": { "type": "string" }, "description": "Add these inline tags (leading # optional); applied after 'tags'" },
                        "remove_tags": { "type": "array", "items": { "type": "string" }, "description": "Remove these inline tags (leading # optional)" }
                    },
                    "required": ["path", "line"]
                }),
            ),
            tool(
                "delete",
                "Permanently remove a task line from a note (destructive; use complete to record completion instead)",
                json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Vault-relative note path (from list)" },
                        "line": { "type": "integer", "minimum": 1, "description": "1-based line of the task (from list)" },
                        "expected_version": { "type": "string", "description": "Optimistic-concurrency token from a prior read; rejected if the note has changed" }
                    },
                    "required": ["path", "line"]
                }),
            ),
            tool(
                "config",
                "Report the Obsidian Tasks settings the module tuned itself to: metadata format, global filter, and where they were resolved from",
                json!({ "type": "object", "properties": {} }),
            ),
        ]
    }

    async fn call_tool(
        &self,
        name: &str,
        arguments: Value,
        context: PluginRequestContext,
    ) -> PluginResult<ToolResult> {
        match name {
            "create" => self.create(&arguments, &context).await,
            "list" => self.list(&arguments).await,
            "overdue" => self.overdue(&arguments).await,
            "tags" => self.tags().await,
            "complete" => self.complete(&arguments, &context).await,
            "update" => self.update(&arguments, &context).await,
            "delete" => self.delete(&arguments, &context).await,
            "config" => self.config_report().await,
            other => Err(PluginError::not_found(format!("unknown tool {other:?}"))),
        }
    }
}

impl TasksProvider {
    /// Resolve the Tasks-plugin settings once, lazily: authoritative
    /// `data.json` first, then a content heuristic, then defaults.
    async fn config(&self) -> &TasksConfig {
        self.config
            .get_or_init(|| async {
                if let Ok(Some(bytes)) = self
                    .vault
                    .read_config(config::OBSIDIAN_TASKS_DATA_PATH)
                    .await
                    && let Some(resolved) = TasksConfig::from_obsidian_data(&bytes)
                {
                    return resolved;
                }
                TasksConfig::from_heuristic(&self.sample_task_text().await)
            })
            .await
    }

    /// Gather a bounded sample of raw task-line text for format detection. Uses
    /// raw note content (not parsed tasks) because the dialect — emoji vs
    /// dataview — is exactly what parsing erases.
    async fn sample_task_text(&self) -> String {
        const MAX_LINES: usize = 400;
        let mut sample = String::new();
        let mut lines = 0usize;
        let Ok(notes) = self.vault.list_notes().await else {
            return sample;
        };
        for path in notes {
            if !path.ends_with(".md") {
                continue;
            }
            let Ok(snapshot) = self.vault.read_note(&path).await else {
                continue;
            };
            for line in snapshot.content.lines() {
                if line.contains("- [") || line.contains("* [") {
                    sample.push_str(line);
                    sample.push('\n');
                    lines += 1;
                    if lines >= MAX_LINES {
                        return sample;
                    }
                }
            }
        }
        sample
    }

    /// Create a new task line in a note. Always injects the global-filter tag
    /// (e.g. `#task`) so the agent never needs to know about it. Renders in the
    /// vault's detected dialect (emoji or Dataview).
    async fn create(
        &self,
        args: &Value,
        context: &PluginRequestContext,
    ) -> PluginResult<ToolResult> {
        let path = required_str(args, "path")?;
        let content = required_str(args, "content")?.to_string();
        if content.trim().is_empty() {
            return Err(PluginError::invalid_input("content must not be empty"));
        }

        let snapshot = self.vault.read_note(path).await?;
        verify_version(args, &snapshot.version)?;

        let (format, global_filter) = {
            let config = self.config().await;
            (config.format, config.global_filter.clone())
        };

        let mut task = TaskItem {
            content,
            is_completed: false,
            status: TaskStatus::Pending,
            position: Default::default(),
            created_date: None,
            scheduled_date: None,
            start_date: None,
            due_date: None,
            done_date: None,
            cancelled_date: None,
            priority: TaskPriority::Normal,
            recurrence: None,
            on_completion: None,
            id: None,
            depends_on: Vec::new(),
            tags: Vec::new(),
            block_ref: None,
            metadata: HashMap::new(),
        };

        apply_edits(&mut task, args)?;

        // Always inject the global-filter tag — the agent never needs to pass it.
        if let Some(filter) = &global_filter
            && let Some(tag) = filter.strip_prefix('#')
        {
            let tag = tag.to_string();
            if !task.tags.iter().any(|t| t.eq_ignore_ascii_case(&tag)) {
                task.tags.push(tag);
            }
        }

        let separator = line_sep(&snapshot.content);
        let mut lines: Vec<String> = snapshot
            .content
            .split(separator)
            .map(str::to_string)
            .collect();

        // Determine insertion line: after the specified line, after the last
        // existing task, or at end of note.
        let insert_after = if let Some(line) = args
            .get("insert_after_line")
            .and_then(Value::as_u64)
            .filter(|n| *n >= 1)
        {
            line as usize
        } else {
            let parsed = turbovault_parser::parse_tasks(&snapshot.content);
            parsed
                .iter()
                .map(|t| t.position.line)
                .max()
                .unwrap_or(lines.len())
        };

        // Borrow indentation and list marker from the line being inserted after.
        let (indent, marker) = if insert_after > 0 && insert_after <= lines.len() {
            split_list_prefix(&lines[insert_after - 1])
        } else {
            (String::new(), "-".to_string())
        };

        let new_line = format!(
            "{indent}{marker} [{}] {}",
            task.status.marker(),
            render::render_body(&task, format)
        );

        let insertion_line = if insert_after >= lines.len() {
            lines.push(new_line.clone());
            lines.len()
        } else {
            lines.insert(insert_after, new_line.clone());
            insert_after + 1
        };

        let new_content = lines.join(separator);

        let receipt = self
            .vault
            .write_note(WriteNoteRequest {
                path: path.to_string(),
                content: new_content,
                precondition: WritePrecondition::Match(snapshot.version.clone()),
                commit_message: Some(format!("tasks: create {path}:{insertion_line}")),
                provenance: Some(provenance(
                    context,
                    format!("create {path}:{insertion_line}"),
                )),
            })
            .await?;

        ok_json(json!({
            "path": path,
            "line": insertion_line,
            "line_text": new_line,
            "format": format_name(format),
            "global_filter_applied": global_filter.is_some(),
            "version": receipt.version,
        }))
    }

    /// Read every markdown note and parse its tasks, tagged with the note path.
    /// Honors the resolved global filter, so results mirror what the user's
    /// Tasks plugin considers a task.
    async fn read_all_tasks(&self) -> PluginResult<Vec<(String, TaskItem)>> {
        // Clone before the vault awaits. Holding `&TasksConfig` from `self.config`
        // across those awaits overlaps the provider borrow.
        let config = self.config().await.clone();

        let notes = self.vault.list_notes().await?;
        let mut out = Vec::new();
        for path in notes {
            if !path.ends_with(".md") {
                continue;
            }
            // A note that fails to read (e.g. vanished between list and read) is
            // skipped rather than failing the whole query.
            let Ok(snapshot) = self.vault.read_note(&path).await else {
                continue;
            };
            for task in turbovault_parser::parse_tasks(&snapshot.content) {
                if config.accepts(&task) {
                    out.push((path.clone(), task));
                }
            }
        }
        Ok(out)
    }

    async fn list(&self, args: &Value) -> PluginResult<ToolResult> {
        let status = get_str(args, "status")
            .unwrap_or("all")
            .to_ascii_lowercase();
        let tag_filters: Vec<String> = args
            .get("tags")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|tag| tag.trim_start_matches('#').to_ascii_lowercase())
                    .filter(|tag| !tag.is_empty())
                    .collect()
            })
            .unwrap_or_default();

        let mut tasks = Vec::new();
        for (path, task) in self.read_all_tasks().await? {
            let passes_status = matches_status_filter(&status, &task);
            if !passes_status {
                continue;
            }
            let passes_tags = tag_filters.iter().all(|required| {
                task.tags
                    .iter()
                    .any(|tag| tag.eq_ignore_ascii_case(required))
            });
            if passes_tags {
                tasks.push(task_json(&path, &task));
            }
        }

        let count = tasks.len();
        ok_json(json!({
            "tasks": tasks,
            "count": count,
            "status_filter": status,
            "tag_filters": tag_filters,
        }))
    }

    async fn overdue(&self, args: &Value) -> PluginResult<ToolResult> {
        let as_of = match get_str(args, "as_of") {
            Some(raw) if !raw.is_empty() => parse_date(raw, "as_of")?,
            _ => jiff::Zoned::now().date(),
        };

        let mut overdue = Vec::new();
        for (path, task) in self.read_all_tasks().await? {
            if !task.status.is_open() {
                continue;
            }
            if let Some(due) = task.due_date
                && due < as_of
            {
                let mut entry = task_json(&path, &task);
                entry["days_overdue"] = json!((as_of - due).get_days());
                overdue.push(entry);
            }
        }
        overdue.sort_by(|a, b| {
            a["due_date"]
                .as_str()
                .unwrap_or("")
                .cmp(b["due_date"].as_str().unwrap_or(""))
        });

        let count = overdue.len();
        ok_json(json!({
            "tasks": overdue,
            "count": count,
            "as_of": as_of.to_string(),
        }))
    }

    async fn tags(&self) -> PluginResult<ToolResult> {
        let mut set: BTreeSet<String> = BTreeSet::new();
        for (_, task) in self.read_all_tasks().await? {
            for tag in task.tags {
                set.insert(tag.to_ascii_lowercase());
            }
        }
        let tags: Vec<&String> = set.iter().collect();
        let count = tags.len();
        ok_json(json!({ "tags": tags, "count": count }))
    }

    async fn complete(
        &self,
        args: &Value,
        context: &PluginRequestContext,
    ) -> PluginResult<ToolResult> {
        let path = required_str(args, "path")?;
        let line = required_line(args)?;
        let done = match get_str(args, "done_date") {
            Some(raw) if !raw.is_empty() => parse_date(raw, "done_date")?,
            _ => jiff::Zoned::now().date(),
        };

        let snapshot = self.vault.read_note(path).await?;
        verify_version(args, &snapshot.version)?;
        let task = task_at_line(&snapshot.content, line, path)?;

        let separator = line_sep(&snapshot.content);
        let mut lines: Vec<String> = snapshot
            .content
            .split(separator)
            .map(str::to_string)
            .collect();
        let idx = line - 1;
        if idx >= lines.len() {
            return Err(PluginError::invalid_input(format!(
                "line {line} is out of range for {path:?}"
            )));
        }
        // The completed line is edited losslessly (surgery), preserving the
        // author's exact formatting.
        let completed_line = mark_line_completed(&lines[idx], done)?;
        lines[idx] = completed_line.clone();

        // Recurrence: spawn the next occurrence as a fresh open task directly
        // below the completed one. There is no source line to copy, so it is
        // rendered — in the vault's detected dialect, with the same indent/marker.
        let mut next_occurrence: Option<String> = None;
        if let Some(rule) = task.recurrence.clone()
            && let Some((next_due, next_scheduled, next_start)) =
                recurrence::compute_next_occurrence(
                    &rule,
                    task.due_date,
                    task.scheduled_date,
                    task.start_date,
                    done,
                )
        {
            let mut next = task.clone();
            next.is_completed = false;
            next.status = TaskStatus::Pending;
            next.done_date = None;
            next.cancelled_date = None;
            next.due_date = next_due;
            next.scheduled_date = next_scheduled;
            next.start_date = next_start;

            let format = self.config().await.format;
            let (indent, marker) = split_list_prefix(&lines[idx]);
            let rendered = format!(
                "{indent}{marker} [ ] {}",
                render::render_body(&next, format)
            );
            lines.insert(idx + 1, rendered.clone());
            next_occurrence = Some(rendered);
        }

        let new_content = lines.join(separator);

        let receipt = self
            .vault
            .write_note(WriteNoteRequest {
                path: path.to_string(),
                content: new_content,
                precondition: WritePrecondition::Match(snapshot.version.clone()),
                commit_message: Some(format!("tasks: complete {path}:{line}")),
                provenance: Some(provenance(context, format!("complete {path}:{line}"))),
            })
            .await?;

        ok_json(json!({
            "path": path,
            "line": line,
            "completed_line": completed_line,
            "done_date": done.to_string(),
            "spawned_next_occurrence": next_occurrence.is_some(),
            "next_occurrence_line": next_occurrence,
            "version": receipt.version,
            "task": task_json(path, &task),
        }))
    }

    async fn update(
        &self,
        args: &Value,
        context: &PluginRequestContext,
    ) -> PluginResult<ToolResult> {
        let path = required_str(args, "path")?;
        let line = required_line(args)?;

        let snapshot = self.vault.read_note(path).await?;
        verify_version(args, &snapshot.version)?;
        let mut task = task_at_line(&snapshot.content, line, path)?;
        apply_edits(&mut task, args)?;

        let format = self.config().await.format;
        let separator = line_sep(&snapshot.content);
        let mut lines: Vec<String> = snapshot
            .content
            .split(separator)
            .map(str::to_string)
            .collect();
        let idx = line - 1;
        if idx >= lines.len() {
            return Err(PluginError::invalid_input(format!(
                "line {line} is out of range for {path:?}"
            )));
        }

        // Preserve the original indentation and list marker; re-render only the
        // checkbox state and the description+metadata tail.
        let (indent, marker) = split_list_prefix(&lines[idx]);
        let box_char = task.status.marker();
        let new_line = format!(
            "{indent}{marker} [{box_char}] {}",
            render::render_body(&task, format)
        );
        lines[idx] = new_line.clone();
        let new_content = lines.join(separator);

        let receipt = self
            .vault
            .write_note(WriteNoteRequest {
                path: path.to_string(),
                content: new_content,
                precondition: WritePrecondition::Match(snapshot.version.clone()),
                commit_message: Some(format!("tasks: update {path}:{line}")),
                provenance: Some(provenance(context, format!("update {path}:{line}"))),
            })
            .await?;

        ok_json(json!({
            "path": path,
            "line": line,
            "line_text": new_line,
            "format": format_name(format),
            "version": receipt.version,
            "task": task_json(path, &task),
        }))
    }

    async fn config_report(&self) -> PluginResult<ToolResult> {
        let config = self.config().await;
        ok_json(json!({
            "format": format_name(config.format),
            "global_filter": config.global_filter,
            "remove_global_filter": config.remove_global_filter,
            "source": config.source.as_str(),
        }))
    }

    async fn delete(
        &self,
        args: &Value,
        context: &PluginRequestContext,
    ) -> PluginResult<ToolResult> {
        let path = required_str(args, "path")?;
        let line = required_line(args)?;

        let snapshot = self.vault.read_note(path).await?;
        verify_version(args, &snapshot.version)?;
        let task = task_at_line(&snapshot.content, line, path)?;

        let separator = line_sep(&snapshot.content);
        let mut lines: Vec<String> = snapshot
            .content
            .split(separator)
            .map(str::to_string)
            .collect();
        let idx = line - 1;
        if idx >= lines.len() {
            return Err(PluginError::invalid_input(format!(
                "line {line} is out of range for {path:?}"
            )));
        }
        lines.remove(idx);
        let new_content = lines.join(separator);

        let receipt = self
            .vault
            .write_note(WriteNoteRequest {
                path: path.to_string(),
                content: new_content,
                precondition: WritePrecondition::Match(snapshot.version.clone()),
                commit_message: Some(format!("tasks: delete {path}:{line}")),
                provenance: Some(provenance(context, format!("delete {path}:{line}"))),
            })
            .await?;

        ok_json(json!({
            "path": path,
            "line": line,
            "deleted_content": task.content,
            "version": receipt.version,
        }))
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn tool(name: &str, description: &str, schema: Value) -> Tool {
    Tool::new(name, description).with_schema(ToolInputSchema::from_value(schema))
}

fn ok_json(value: Value) -> PluginResult<ToolResult> {
    ToolResult::json(&value).map_err(|error| PluginError::internal(error.to_string()))
}

fn get_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

fn required_str<'a>(args: &'a Value, key: &str) -> PluginResult<&'a str> {
    get_str(args, key).ok_or_else(|| PluginError::invalid_input(format!("{key} is required")))
}

fn required_line(args: &Value) -> PluginResult<usize> {
    args.get("line")
        .and_then(Value::as_u64)
        .filter(|n| *n >= 1)
        .map(|n| n as usize)
        .ok_or_else(|| PluginError::invalid_input("line is required and must be >= 1"))
}

fn verify_version(args: &Value, actual: &str) -> PluginResult<()> {
    if let Some(expected) = get_str(args, "expected_version")
        && expected != actual
    {
        return Err(PluginError::conflict(
            "expected_version does not match the current note; re-read before editing",
        ));
    }
    Ok(())
}

fn parse_date(raw: &str, field: &str) -> PluginResult<Date> {
    raw.parse::<Date>().map_err(|_| {
        PluginError::invalid_input(format!("invalid {field} {raw:?} — use YYYY-MM-DD"))
    })
}

fn provenance(context: &PluginRequestContext, note: String) -> WriteProvenance {
    WriteProvenance {
        source: "tasks-plugin".to_string(),
        correlation_id: Some(context.request_id.clone()),
        note: Some(note),
    }
}

/// `"\r\n"` if the note uses Windows line endings, else `"\n"`.
fn line_sep(content: &str) -> &'static str {
    if content.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    }
}

/// Find the parsed task on `line` (1-based), erroring if none is there.
fn task_at_line(content: &str, line: usize, path: &str) -> PluginResult<TaskItem> {
    turbovault_parser::parse_tasks(content)
        .into_iter()
        .find(|task| task.position.line == line)
        .ok_or_else(|| PluginError::invalid_input(format!("no task at {path}:{line}")))
}

/// Flip a task line's checkbox to `[x]` and append `✅ <done>` if absent.
///
/// Operates directly on the source line so no task metadata is lost to a
/// round-trip through `TaskItem`.
fn mark_line_completed(line: &str, done: Date) -> PluginResult<String> {
    let bytes = line.as_bytes();
    let mut checkbox = None;
    let mut i = 0;
    while i + 2 < bytes.len() {
        if bytes[i] == b'[' && bytes[i + 2] == b']' {
            checkbox = Some(i);
            break;
        }
        i += 1;
    }
    let open = checkbox.ok_or_else(|| {
        PluginError::invalid_input(format!("line is not a task (no checkbox): {line:?}"))
    })?;
    let status = open + 1;

    let mut result = String::with_capacity(line.len() + 16);
    result.push_str(&line[..status]);
    result.push('x');
    result.push_str(&line[status + 1..]);
    if !result.contains('✅') {
        result.push_str(&format!(" ✅ {done}"));
    }
    Ok(result)
}

fn format_name(format: TaskFormat) -> &'static str {
    match format {
        TaskFormat::Emoji => "emoji",
        TaskFormat::Dataview => "dataview",
    }
}

/// Split a list line into (leading indent, list marker) so an edit can rewrite
/// the content while preserving the author's indentation and bullet style.
fn split_list_prefix(line: &str) -> (String, String) {
    let indent: String = line
        .chars()
        .take_while(|character| *character == ' ' || *character == '\t')
        .collect();
    let rest = &line[indent.len()..];

    for bullet in ['-', '*', '+'] {
        if rest.starts_with(bullet) && rest[1..].starts_with(' ') {
            return (indent, bullet.to_string());
        }
    }
    // Ordered list: digits followed by `.` or `)`.
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if !digits.is_empty() {
        let after = &rest[digits.len()..];
        if after.starts_with(". ") {
            return (indent, format!("{digits}."));
        }
        if after.starts_with(") ") {
            return (indent, format!("{digits})"));
        }
    }
    (indent, "-".to_string())
}

/// Apply the optional edit fields from `args` onto a parsed task. Absent keys
/// leave a field unchanged; `null` or `""` clears an optional field.
fn apply_edits(task: &mut TaskItem, args: &Value) -> PluginResult<()> {
    if let Some(value) = args.get("content") {
        task.content = value
            .as_str()
            .ok_or_else(|| PluginError::invalid_input("content must be a string"))?
            .to_string();
    }
    if let Some(value) = args.get("completed") {
        let completed = value
            .as_bool()
            .ok_or_else(|| PluginError::invalid_input("completed must be a boolean"))?;
        task.is_completed = completed;
        // Completing is Done. Clearing completion returns to todo, including
        // from in-progress or cancelled.
        task.status = if completed {
            TaskStatus::Done
        } else {
            TaskStatus::Pending
        };
    }
    if let Some(value) = args.get("priority") {
        let word = value
            .as_str()
            .ok_or_else(|| PluginError::invalid_input("priority must be a string"))?;
        task.priority = parse_priority_word(word)?;
    }

    edit_date(&mut task.due_date, args, "due")?;
    edit_date(&mut task.scheduled_date, args, "scheduled")?;
    edit_date(&mut task.start_date, args, "start")?;
    edit_date(&mut task.created_date, args, "created")?;
    edit_date(&mut task.done_date, args, "done")?;
    edit_date(&mut task.cancelled_date, args, "cancelled")?;

    if let Some(value) = args.get("recurrence") {
        task.recurrence = match value {
            Value::Null => None,
            Value::String(text) if text.trim().is_empty() => None,
            Value::String(text) => Some(text.trim().to_string()),
            _ => {
                return Err(PluginError::invalid_input(
                    "recurrence must be a string or null",
                ));
            }
        };
    }

    // `tags` replaces the whole set; `add_tags`/`remove_tags` edit it
    // incrementally. When combined, the replace is applied first.
    if let Some(value) = args.get("tags") {
        task.tags = string_array(value, "tags")?
            .into_iter()
            .map(|raw| raw.trim().trim_start_matches('#').to_string())
            .filter(|tag| !tag.is_empty())
            .collect();
    }
    if let Some(value) = args.get("add_tags") {
        for raw in string_array(value, "add_tags")? {
            let clean = raw.trim().trim_start_matches('#');
            if !clean.is_empty() && !task.tags.iter().any(|tag| tag.eq_ignore_ascii_case(clean)) {
                task.tags.push(clean.to_string());
            }
        }
    }
    if let Some(value) = args.get("remove_tags") {
        let removals: Vec<String> = string_array(value, "remove_tags")?
            .into_iter()
            .map(|raw| raw.trim().trim_start_matches('#').to_ascii_lowercase())
            .collect();
        task.tags.retain(|tag| {
            !removals
                .iter()
                .any(|removal| removal.eq_ignore_ascii_case(tag))
        });
    }

    Ok(())
}

fn string_array<'a>(value: &'a Value, key: &str) -> PluginResult<Vec<&'a str>> {
    value
        .as_array()
        .ok_or_else(|| PluginError::invalid_input(format!("{key} must be an array of strings")))?
        .iter()
        .map(|item| {
            item.as_str()
                .ok_or_else(|| PluginError::invalid_input(format!("{key} must be strings")))
        })
        .collect()
}

fn edit_date(field: &mut Option<Date>, args: &Value, key: &str) -> PluginResult<()> {
    match args.get(key) {
        None => {}
        Some(Value::Null) => *field = None,
        Some(Value::String(text)) if text.trim().is_empty() => *field = None,
        Some(Value::String(text)) => *field = Some(parse_date(text, key)?),
        Some(_) => {
            return Err(PluginError::invalid_input(format!(
                "{key} must be a YYYY-MM-DD string or null"
            )));
        }
    }
    Ok(())
}

fn parse_priority_word(word: &str) -> PluginResult<TaskPriority> {
    Ok(match word.trim().to_ascii_lowercase().as_str() {
        "highest" => TaskPriority::Highest,
        "high" => TaskPriority::High,
        "medium" => TaskPriority::Medium,
        "low" => TaskPriority::Low,
        "lowest" => TaskPriority::Lowest,
        "normal" | "none" | "" => TaskPriority::Normal,
        other => {
            return Err(PluginError::invalid_input(format!(
                "unknown priority {other:?}; use highest|high|medium|low|lowest|normal"
            )));
        }
    })
}

/// `open` / `pending` / `todo` / `incomplete` are todo and in progress.
/// Cancelled is its own filter. Unknown values, including `all`, match every status.
fn matches_status_filter(status_filter: &str, task: &TaskItem) -> bool {
    match status_filter {
        "completed" | "done" => task.status == TaskStatus::Done,
        "pending" | "open" | "todo" | "incomplete" => task.status.is_open(),
        "cancelled" => task.status == TaskStatus::Cancelled,
        _ => true,
    }
}

fn task_json(path: &str, task: &TaskItem) -> Value {
    json!({
        "path": path,
        "line": task.position.line,
        "content": task.content,
        "is_completed": task.is_completed,
        "status": task.status.as_str(),
        "priority": task.priority,
        "due_date": task.due_date.map(|date| date.to_string()),
        "scheduled_date": task.scheduled_date.map(|date| date.to_string()),
        "start_date": task.start_date.map(|date| date.to_string()),
        "done_date": task.done_date.map(|date| date.to_string()),
        "recurrence": task.recurrence,
        "tags": task.tags,
        "depends_on": task.depends_on,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_line_separator() {
        assert_eq!(line_sep("a\nb"), "\n");
        assert_eq!(line_sep("a\r\nb"), "\r\n");
    }

    #[test]
    fn completes_a_plain_task_line() {
        let out = mark_line_completed("- [ ] buy milk", "2026-07-18".parse().unwrap()).unwrap();
        assert_eq!(out, "- [x] buy milk ✅ 2026-07-18");
    }

    #[test]
    fn completes_an_indented_task_and_preserves_metadata() {
        let out = mark_line_completed(
            "    - [ ] pay rent 📅 2026-07-01",
            "2026-07-18".parse().unwrap(),
        )
        .unwrap();
        assert_eq!(out, "    - [x] pay rent 📅 2026-07-01 ✅ 2026-07-18");
    }

    #[test]
    fn does_not_double_stamp_done_date() {
        let out = mark_line_completed(
            "- [/] in progress ✅ 2026-07-10",
            "2026-07-18".parse().unwrap(),
        )
        .unwrap();
        assert_eq!(out, "- [x] in progress ✅ 2026-07-10");
    }

    #[test]
    fn rejects_a_non_task_line() {
        assert!(mark_line_completed("just a paragraph", "2026-07-18".parse().unwrap()).is_err());
    }

    #[test]
    fn status_filter_treats_cancelled_as_not_open() {
        let note = "- [ ] todo item\n- [/] working\n- [-] dropped\n- [x] finished\n";
        let tasks = turbovault_parser::parse_tasks(note);
        assert_eq!(tasks.len(), 4);
        let status_of = |content: &str| {
            tasks
                .iter()
                .find(|task| task.content == content)
                .unwrap_or_else(|| panic!("missing {content}"))
                .status
        };
        assert_eq!(status_of("todo item"), TaskStatus::Pending);
        assert_eq!(status_of("working"), TaskStatus::InProgress);
        assert_eq!(status_of("dropped"), TaskStatus::Cancelled);
        assert_eq!(status_of("finished"), TaskStatus::Done);

        let contents = |filter: &str| -> Vec<&str> {
            tasks
                .iter()
                .filter(|task| matches_status_filter(filter, task))
                .map(|task| task.content.as_str())
                .collect()
        };
        assert_eq!(contents("open"), ["todo item", "working"]);
        assert_eq!(contents("pending"), ["todo item", "working"]);
        assert_eq!(contents("todo"), ["todo item", "working"]);
        assert_eq!(contents("incomplete"), ["todo item", "working"]);
        assert_eq!(contents("cancelled"), ["dropped"]);
        assert_eq!(contents("done"), ["finished"]);
        assert_eq!(contents("completed"), ["finished"]);
        assert_eq!(contents("all").len(), 4);
    }

    #[test]
    fn completed_edit_keeps_status_consistent() {
        let mut task = turbovault_parser::parse_tasks("- [/] working")
            .into_iter()
            .next()
            .unwrap();
        apply_edits(&mut task, &json!({"completed": true})).unwrap();
        assert!(task.is_completed);
        assert_eq!(task.status, TaskStatus::Done);

        apply_edits(&mut task, &json!({"completed": false})).unwrap();
        assert!(!task.is_completed);
        assert_eq!(task.status, TaskStatus::Pending);
        assert!(task.status.is_open());
    }
}
