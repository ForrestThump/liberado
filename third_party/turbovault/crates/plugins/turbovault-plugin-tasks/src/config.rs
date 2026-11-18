//! Self-tuning configuration for the `tasks` module.
//!
//! The Obsidian Tasks plugin records two things this module needs in order to
//! behave like the user's own vault: the **metadata format** (emoji vs Dataview
//! inline fields) it writes, and the **global filter** — a tag that marks which
//! checkbox lines are "real" tasks. Both live in
//! `.obsidian/plugins/obsidian-tasks-plugin/data.json`.
//!
//! We resolve the config in priority order:
//! 1. the authoritative `data.json`, read through the curated
//!    [`VaultApi::read_config`](turbovault_plugin_api::VaultApi::read_config);
//! 2. a **content heuristic** over the vault's existing task lines, when the
//!    settings file is absent or the host declines config reads;
//! 3. a plain default (emoji, no global filter).

use serde_json::Value;
use turbovault_core::TaskItem;

/// Which trailing-metadata dialect the user's Tasks plugin reads and writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TaskFormat {
    /// `📅 2026-07-19 ⏫ 🔁 every week` — the Tasks plugin's emoji signifiers.
    ///
    /// This is the Tasks plugin's own default (`tasksPluginEmoji`).
    #[default]
    Emoji,
    /// `[due:: 2026-07-19] [priority:: high]` — Dataview inline fields.
    Dataview,
}

/// Where a resolved [`TasksConfig`] came from — surfaced by `tasks_config` so
/// the sync is observable rather than magic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigSource {
    /// Parsed from the Tasks plugin's `data.json`.
    ObsidianData,
    /// Inferred from the vault's existing task lines.
    Heuristic,
    /// Nothing to go on; built-in defaults.
    Default,
}

impl ConfigSource {
    pub fn as_str(self) -> &'static str {
        match self {
            ConfigSource::ObsidianData => "obsidian-data",
            ConfigSource::Heuristic => "heuristic",
            ConfigSource::Default => "default",
        }
    }
}

/// Resolved Tasks-plugin settings the module tunes itself to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TasksConfig {
    pub format: TaskFormat,
    /// Global-filter tag *with* its leading `#` (e.g. `#task`), if the user set
    /// one. When present, only checkbox lines bearing this tag are treated as
    /// tasks — mirroring the Tasks plugin.
    pub global_filter: Option<String>,
    /// Whether the Tasks plugin hides the global-filter tag from the rendered
    /// description.
    pub remove_global_filter: bool,
    pub source: ConfigSource,
}

impl Default for TasksConfig {
    fn default() -> Self {
        Self {
            format: TaskFormat::default(),
            global_filter: None,
            remove_global_filter: false,
            source: ConfigSource::Default,
        }
    }
}

/// Canonical vault-relative path of the Tasks plugin settings file.
pub const OBSIDIAN_TASKS_DATA_PATH: &str = ".obsidian/plugins/obsidian-tasks-plugin/data.json";

impl TasksConfig {
    /// Parse the Tasks plugin's `data.json`. Returns `None` if the bytes are not
    /// the JSON object we expect, so the caller can fall back to heuristics.
    pub fn from_obsidian_data(bytes: &[u8]) -> Option<TasksConfig> {
        let value: Value = serde_json::from_slice(bytes).ok()?;
        let object = value.as_object()?;

        // `taskFormat` was introduced with Dataview support; older installs omit
        // it and are emoji by definition.
        let format = match object.get("taskFormat").and_then(Value::as_str) {
            Some("dataview") => TaskFormat::Dataview,
            _ => TaskFormat::Emoji,
        };

        let global_filter = object
            .get("globalFilter")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|filter| !filter.is_empty())
            .map(str::to_string);

        let remove_global_filter = object
            .get("removeGlobalFilter")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        Some(TasksConfig {
            format,
            global_filter,
            remove_global_filter,
            source: ConfigSource::ObsidianData,
        })
    }

    /// Best-effort config inferred from raw task-bearing note text when the
    /// settings file is unavailable. Only the format is inferable; a global
    /// filter that is not already on tasks cannot be discovered this way.
    pub fn from_heuristic(sample: &str) -> TasksConfig {
        match detect_format(sample) {
            Some(format) => TasksConfig {
                format,
                global_filter: None,
                remove_global_filter: false,
                source: ConfigSource::Heuristic,
            },
            None => TasksConfig::default(),
        }
    }

    /// Whether `task` survives the global filter.
    ///
    /// No filter accepts every task. A `#tag` filter matches that tag token on
    /// `task.tags`, case-insensitively (`#task` does not match `#tasks`). Any
    /// other filter is a case-insensitive substring of the task description.
    #[must_use]
    pub fn accepts(&self, task: &TaskItem) -> bool {
        let Some(filter) = &self.global_filter else {
            return true;
        };
        if filter.is_empty() {
            return true;
        }
        if let Some(tag) = filter.strip_prefix('#') {
            task.tags
                .iter()
                .any(|value| value.eq_ignore_ascii_case(tag))
        } else {
            task.content
                .to_ascii_lowercase()
                .contains(&filter.to_ascii_lowercase())
        }
    }

    /// Description shown when the Tasks plugin hides the global filter.
    ///
    /// Returns `task.content` unchanged unless `remove_global_filter` is set.
    /// A `#tag` filter drops a whitespace-delimited `#tag` token (so `#tasks`
    /// and `#task/sub` stay intact). Any other filter is removed as a
    /// case-insensitive substring.
    #[must_use]
    pub fn display_text(&self, task: &TaskItem) -> String {
        if !self.remove_global_filter {
            return task.content.clone();
        }
        let Some(filter) = self.global_filter.as_deref() else {
            return task.content.clone();
        };
        if filter.is_empty() {
            return task.content.clone();
        }
        let stripped = if let Some(tag) = filter.strip_prefix('#') {
            strip_tag_token(&task.content, tag)
        } else {
            remove_substring_ci(&task.content, filter)
        };
        collapse_whitespace(&stripped)
    }
}

/// Drop whitespace-delimited `#tag` tokens. A longer token such as `#tasks`
/// or `#task/sub` is left alone.
fn strip_tag_token(content: &str, tag: &str) -> String {
    let needle = format!("#{tag}");
    content
        .split_whitespace()
        .filter(|token| !token.eq_ignore_ascii_case(&needle))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Remove every case-insensitive occurrence of `needle`. ASCII case folding
/// does not change the byte length, so the match index is valid on `content`.
fn remove_substring_ci(content: &str, needle: &str) -> String {
    if needle.is_empty() {
        return content.to_string();
    }
    let needle_lower = needle.to_ascii_lowercase();
    let mut out = String::with_capacity(content.len());
    let mut rest = content;
    let mut rest_lower = content.to_ascii_lowercase();
    while let Some(index) = rest_lower.find(&needle_lower) {
        out.push_str(&rest[..index]);
        let next = index + needle_lower.len();
        rest = &rest[next..];
        rest_lower = rest_lower[next..].to_string();
    }
    out.push_str(rest);
    out
}

fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The emoji signifiers the parser recognizes; their presence implies emoji
/// format.
const FORMAT_EMOJIS: [&str; 7] = ["📅", "⏳", "🛫", "➕", "✅", "❌", "🔁"];

/// Infer the metadata dialect by counting emoji signifiers against Dataview
/// inline fields across the sample. `None` when there is no signal either way.
fn detect_format(sample: &str) -> Option<TaskFormat> {
    let emoji = FORMAT_EMOJIS
        .iter()
        .map(|marker| sample.matches(marker).count())
        .sum::<usize>();

    let dataview = [
        "[due::",
        "[scheduled::",
        "[start::",
        "[created::",
        "[completion::",
        "[priority::",
        "[repeat::",
        "[recurrence::",
    ]
    .iter()
    .map(|marker| sample.matches(marker).count())
    .sum::<usize>();

    match (emoji, dataview) {
        (0, 0) => None,
        (e, d) if d > e => Some(TaskFormat::Dataview),
        _ => Some(TaskFormat::Emoji),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_emoji_default_and_global_filter_from_data_json() {
        let json = br##"{ "globalFilter": "#task", "removeGlobalFilter": true }"##;
        let config = TasksConfig::from_obsidian_data(json).expect("parse");
        assert_eq!(config.format, TaskFormat::Emoji);
        assert_eq!(config.global_filter.as_deref(), Some("#task"));
        assert!(config.remove_global_filter);
        assert_eq!(config.source, ConfigSource::ObsidianData);
    }

    #[test]
    fn parses_dataview_format() {
        let json = br#"{ "taskFormat": "dataview", "globalFilter": "" }"#;
        let config = TasksConfig::from_obsidian_data(json).expect("parse");
        assert_eq!(config.format, TaskFormat::Dataview);
        assert_eq!(config.global_filter, None);
    }

    #[test]
    fn rejects_non_json() {
        assert!(TasksConfig::from_obsidian_data(b"not json").is_none());
    }

    #[test]
    fn heuristic_detects_dataview_over_emoji() {
        let sample = "- [ ] a [due:: 2026-01-01]\n- [ ] b [priority:: high]";
        assert_eq!(
            TasksConfig::from_heuristic(sample).format,
            TaskFormat::Dataview
        );
        assert_eq!(
            TasksConfig::from_heuristic(sample).source,
            ConfigSource::Heuristic
        );
    }

    #[test]
    fn heuristic_detects_emoji() {
        let sample = "- [ ] a 📅 2026-01-01 ⏫";
        assert_eq!(
            TasksConfig::from_heuristic(sample).format,
            TaskFormat::Emoji
        );
    }

    #[test]
    fn heuristic_falls_back_to_default_without_signal() {
        assert_eq!(
            TasksConfig::from_heuristic("- [ ] plain task").source,
            ConfigSource::Default
        );
    }

    fn task_line(line: &str) -> turbovault_core::TaskItem {
        turbovault_parser::parse_tasks(line)
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("expected a task in {line:?}"))
    }

    #[test]
    fn accepts_tag_filter_is_a_case_insensitive_token_match() {
        let config = TasksConfig::from_obsidian_data(
            br##"{"globalFilter":"#task","removeGlobalFilter":false}"##,
        )
        .unwrap();

        assert!(config.accepts(&task_line("- [ ] Real work #task")));
        assert!(config.accepts(&task_line("- [ ] Real work #Task")));
        assert!(!config.accepts(&task_line("- [ ] Grouped #tasks")));
        assert!(!config.accepts(&task_line("- [ ] Force #taskforce")));
        assert!(!config.accepts(&task_line("- [ ] Mention the word task in prose")));
    }

    #[test]
    fn accepts_non_tag_filter_is_a_content_substring() {
        let config = TasksConfig::from_obsidian_data(br#"{"globalFilter":"rollover"}"#).unwrap();
        assert!(config.accepts(&task_line("- [ ] CNH retirement account rollover")));
        assert!(config.accepts(&task_line("- [ ] ROLLOVER the funds")));
        assert!(!config.accepts(&task_line("- [ ] Something else")));
    }

    #[test]
    fn accepts_everything_when_there_is_no_filter() {
        let config = TasksConfig::default();
        assert!(config.accepts(&task_line("- [ ] no tag")));
        assert!(config.accepts(&task_line("- [ ] has #task")));
    }

    #[test]
    fn cnh_copies_without_the_global_filter_are_not_open_tasks() {
        let config = TasksConfig::from_obsidian_data(
            br##"{"globalFilter":"#task","removeGlobalFilter":false}"##,
        )
        .unwrap();
        let note = "\
- [x] Look into CNH retirement account rollover #task ✅ 2026-09-12
- [ ] Look into CNH retirement account rollover
- [ ] Look into CNH retirement account rollover
- [ ] Something else #task
";
        let open: Vec<String> = turbovault_parser::parse_tasks(note)
            .into_iter()
            .filter(|task| config.accepts(task))
            .filter(|task| task.status.is_open())
            .map(|task| task.content)
            .collect();
        assert_eq!(open, vec!["Something else".to_string()]);
    }

    #[test]
    fn display_text_strips_only_the_global_filter_token() {
        let mut config = TasksConfig::from_obsidian_data(
            br##"{"globalFilter":"#task","removeGlobalFilter":true}"##,
        )
        .unwrap();
        let mut task = task_line("- [ ] Keep this");
        task.content = "Keep #task this #tasks and #task/sub".to_string();
        assert_eq!(config.display_text(&task), "Keep this #tasks and #task/sub");

        config.remove_global_filter = false;
        assert_eq!(
            config.display_text(&task),
            "Keep #task this #tasks and #task/sub"
        );

        config.remove_global_filter = true;
        config.global_filter = Some("rollover".to_string());
        task.content = "CNH retirement account rollover plan".to_string();
        assert_eq!(config.display_text(&task), "CNH retirement account plan");
    }
}
