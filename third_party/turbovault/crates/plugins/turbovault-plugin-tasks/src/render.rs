//! `TaskItem` → Obsidian-Markdown rendering — the write half of the round trip
//! whose read half (`turbovault_parser::parse_tasks`) lives in core.
//!
//! This is the module's own opinion: parsing is tolerant and belongs to the
//! kernel, but *emitting* a task must choose one canonical dialect, and that
//! choice (emoji vs Dataview) is tuned to the user's Tasks-plugin settings. The
//! renderer is the sole consumer of that opinion, so it lives here rather than
//! in core.
//!
//! Rendering canonicalizes field order; it is **not** byte-identical to the
//! source line. The invariant it guarantees is semantic:
//! `parse_tasks(render(task)) == task` for every field the parser understands.

use turbovault_core::{TaskItem, TaskPriority};

use crate::config::TaskFormat;

/// Render a complete task line, including the list marker and checkbox, using a
/// canonical `- ` bullet. Callers editing an existing line should instead reuse
/// its original indent/marker and call [`render_body`].
pub fn to_markdown_line(task: &TaskItem, format: TaskFormat) -> String {
    let box_char = task.status.marker();
    format!("- [{box_char}] {}", render_body(task, format))
}

/// Render the description-plus-metadata tail of a task (no list marker/checkbox).
///
/// Order: description, priority, recurrence, dates (created → start → scheduled
/// → due → cancelled → done), on-completion, id, depends-on, custom Dataview
/// fields, `#tags`, then the `^block-ref` — which Obsidian requires last.
pub fn render_body(task: &TaskItem, format: TaskFormat) -> String {
    let mut parts: Vec<String> = Vec::new();

    if !task.content.is_empty() {
        parts.push(task.content.clone());
    }

    // Priority: a bare emoji, or a Dataview word (Normal renders nothing).
    match format {
        TaskFormat::Emoji => {
            if task.priority != TaskPriority::Normal {
                parts.push(task.priority.emoji().to_string());
            }
        }
        TaskFormat::Dataview => {
            if let Some(word) = priority_word(task.priority) {
                parts.push(format!("[priority:: {word}]"));
            }
        }
    }

    if let Some(recurrence) = &task.recurrence {
        parts.push(scalar(format, "🔁", "repeat", recurrence));
    }

    parts.extend(dated(format, "➕", "created", task.created_date));
    parts.extend(dated(format, "🛫", "start", task.start_date));
    parts.extend(dated(format, "⏳", "scheduled", task.scheduled_date));
    parts.extend(dated(format, "📅", "due", task.due_date));
    parts.extend(dated(format, "❌", "cancelled", task.cancelled_date));
    // `completion` is the Tasks plugin's canonical dataview key for the done date.
    parts.extend(dated(format, "✅", "completion", task.done_date));

    if let Some(on_completion) = &task.on_completion {
        parts.push(scalar(format, "🏁", "onCompletion", on_completion));
    }
    if let Some(id) = &task.id {
        parts.push(scalar(format, "🆔", "id", id));
    }
    if !task.depends_on.is_empty() {
        parts.push(scalar(
            format,
            "⛔",
            "dependsOn",
            &task.depends_on.join(","),
        ));
    }

    // Custom Dataview fields the parser preserved but does not model as typed
    // fields — always emitted in inline-field form, sorted for determinism.
    // Standard keys are skipped here: the parser mirrors dataview-authored
    // standard fields into `metadata` *and* the typed fields above, so emitting
    // them from both would duplicate them.
    let mut custom: Vec<(&String, &String)> = task
        .metadata
        .iter()
        .filter(|(key, _)| !is_standard_key(key))
        .collect();
    custom.sort_by(|a, b| a.0.cmp(b.0));
    for (key, value) in custom {
        parts.push(format!("[{key}:: {value}]"));
    }

    for tag in &task.tags {
        parts.push(format!("#{tag}"));
    }
    if let Some(block_ref) = &task.block_ref {
        parts.push(format!("^{block_ref}"));
    }

    parts.join(" ")
}

/// A single scalar field: `emoji value` or `[key:: value]`.
fn scalar(format: TaskFormat, emoji: &str, key: &str, value: &str) -> String {
    match format {
        TaskFormat::Emoji => format!("{emoji} {value}"),
        TaskFormat::Dataview => format!("[{key}:: {value}]"),
    }
}

/// An optional date field, rendered only when present.
fn dated(
    format: TaskFormat,
    emoji: &str,
    key: &str,
    date: Option<jiff::civil::Date>,
) -> Option<String> {
    date.map(|date| scalar(format, emoji, key, &date.to_string()))
}

/// Standard Tasks metadata keys (as the parser normalizes them). These are
/// modeled as typed fields, so they must not be re-emitted from `metadata`.
fn is_standard_key(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "due"
            | "scheduled"
            | "start"
            | "done"
            | "completion"
            | "cancelled"
            | "canceled"
            | "created"
            | "recurrence"
            | "repeat"
            | "oncompletion"
            | "id"
            | "dependson"
            | "priority"
    )
}

fn priority_word(priority: TaskPriority) -> Option<&'static str> {
    match priority {
        TaskPriority::Highest => Some("highest"),
        TaskPriority::High => Some("high"),
        TaskPriority::Medium => Some("medium"),
        TaskPriority::Low => Some("low"),
        TaskPriority::Lowest => Some("lowest"),
        TaskPriority::Normal => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use turbovault_parser::parse_tasks;

    /// The load-bearing invariant: re-parsing a rendered task reproduces every
    /// field the parser models. Position is excluded (it depends on file layout).
    fn assert_round_trips(line: &str, format: TaskFormat) {
        let original = parse_tasks(line).into_iter().next().expect("a task");
        let rendered = to_markdown_line(&original, format);
        let reparsed = parse_tasks(&rendered)
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("rendered line did not parse as a task: {rendered:?}"));

        assert_eq!(original.content, reparsed.content, "content ({rendered:?})");
        assert_eq!(original.status, reparsed.status, "status ({rendered:?})");
        assert_eq!(
            original.is_completed, reparsed.is_completed,
            "completed ({rendered:?})"
        );
        assert_eq!(
            original.priority, reparsed.priority,
            "priority ({rendered:?})"
        );
        assert_eq!(original.due_date, reparsed.due_date, "due ({rendered:?})");
        assert_eq!(
            original.scheduled_date, reparsed.scheduled_date,
            "scheduled ({rendered:?})"
        );
        assert_eq!(
            original.start_date, reparsed.start_date,
            "start ({rendered:?})"
        );
        assert_eq!(
            original.created_date, reparsed.created_date,
            "created ({rendered:?})"
        );
        assert_eq!(
            original.done_date, reparsed.done_date,
            "done ({rendered:?})"
        );
        assert_eq!(
            original.cancelled_date, reparsed.cancelled_date,
            "cancelled ({rendered:?})"
        );
        assert_eq!(
            original.recurrence, reparsed.recurrence,
            "recurrence ({rendered:?})"
        );
        assert_eq!(
            original.on_completion, reparsed.on_completion,
            "on_completion ({rendered:?})"
        );
        assert_eq!(original.id, reparsed.id, "id ({rendered:?})");
        assert_eq!(
            original.depends_on, reparsed.depends_on,
            "depends_on ({rendered:?})"
        );
        assert_eq!(original.tags, reparsed.tags, "tags ({rendered:?})");
        assert_eq!(
            original.block_ref, reparsed.block_ref,
            "block_ref ({rendered:?})"
        );
        // Standard fields are compared via the typed fields above; the parser
        // also mirrors them into `metadata` under whatever alias the author used
        // (e.g. `completion` vs `done`), which is redundant and not semantically
        // load-bearing. Only custom fields must survive in the map.
        assert_eq!(
            custom_only(&original),
            custom_only(&reparsed),
            "custom metadata ({rendered:?})"
        );
    }

    fn custom_only(task: &TaskItem) -> std::collections::BTreeMap<String, String> {
        task.metadata
            .iter()
            .filter(|(key, _)| !is_standard_key(key))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    }

    #[test]
    fn emoji_round_trips() {
        for line in [
            "- [ ] Buy milk 📅 2026-05-15 ⏫ #errand",
            "- [x] Ship it ✅ 2026-07-10 🔁 every week #work ^ship-1",
            "- [/] Still writing 📅 2026-05-15",
            "- [-] Dropped 📅 2026-05-15",
            "- [ ] Blocked 🏁 keep 🆔 dcf64c ⛔ abc123,def456",
            "- [ ] Bare description with no metadata",
        ] {
            assert_round_trips(line, TaskFormat::Emoji);
        }
    }

    #[test]
    fn dataview_round_trips() {
        for line in [
            "- [ ] Finish report [due:: 2026-06-01] [priority:: high] #work",
            "- [x] Ship it [completion:: 2026-07-01] [repeat:: every month]",
            "- [ ] Custom [project:: [[Team Work]]] [due:: 2026-05-20]",
        ] {
            assert_round_trips(line, TaskFormat::Dataview);
        }
    }

    /// Format is a rendering choice, not a property of the source: an
    /// emoji-authored task re-rendered as Dataview keeps the same fields.
    #[test]
    fn format_is_independent_of_source_dialect() {
        let emoji = parse_tasks("- [ ] Cross 📅 2026-05-15 ⏫ 🔁 every day")
            .into_iter()
            .next()
            .unwrap();
        let as_dataview = to_markdown_line(&emoji, TaskFormat::Dataview);
        assert!(as_dataview.contains("[due:: 2026-05-15]"), "{as_dataview}");
        assert!(as_dataview.contains("[priority:: high]"), "{as_dataview}");
        assert!(
            as_dataview.contains("[repeat:: every day]"),
            "{as_dataview}"
        );

        let reparsed = parse_tasks(&as_dataview).into_iter().next().unwrap();
        assert_eq!(reparsed.due_date, emoji.due_date);
        assert_eq!(reparsed.priority, emoji.priority);
        assert_eq!(reparsed.recurrence, emoji.recurrence);
    }
}
