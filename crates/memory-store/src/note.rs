use crate::error::MemoryError;
use chrono::{DateTime, Utc};
use liberado_common::frontmatter::{body_after_frontmatter, extract_frontmatter, render_note};
use serde::{Deserialize, Serialize};

/// A single memory note: structured metadata in frontmatter, the actual memory/guidance text as
/// the body (so it reads as plain markdown, not YAML-escaped, and is what gets chunked/embedded
/// for vector search). `task_type`/`tools_used`/`success` are only ever set on procedural
/// (tool-guidance) notes; general notes leave them `None`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryNote {
    pub id: String,
    pub created: DateTime<Utc>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools_used: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub success: Option<bool>,
    #[serde(skip)]
    pub content: String,
}

impl MemoryNote {
    pub fn general(id: impl Into<String>, content: impl Into<String>, tags: Vec<String>) -> Self {
        Self {
            id: id.into(),
            created: Utc::now(),
            tags,
            task_type: None,
            tools_used: None,
            success: None,
            content: content.into(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn procedural(
        id: impl Into<String>,
        content: impl Into<String>,
        task_type: Option<String>,
        tools_used: Option<Vec<String>>,
        tags: Vec<String>,
    ) -> Self {
        Self {
            id: id.into(),
            created: Utc::now(),
            tags,
            task_type,
            tools_used,
            success: Some(true),
            content: content.into(),
        }
    }

    /// Render as a markdown note: YAML frontmatter (metadata) + the memory/guidance text as body.
    pub fn to_note_text(&self) -> String {
        render_note(self, &format!("{}\n", self.content))
    }

    /// Parse a memory note back from its rendered text (frontmatter + body).
    pub fn from_note_text(text: &str) -> Result<Self, MemoryError> {
        let frontmatter = extract_frontmatter(text).ok_or(MemoryError::MissingFrontmatter)?;
        let mut note: MemoryNote = serde_yaml::from_str(frontmatter)?;
        note.content = body_after_frontmatter(text).trim().to_string();
        Ok(note)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn general_note_round_trips() {
        let note = MemoryNote::general("mem-1", "User prefers dark mode.", vec!["ui".into()]);
        let text = note.to_note_text();
        let back = MemoryNote::from_note_text(&text).unwrap();
        assert_eq!(back.id, "mem-1");
        assert_eq!(back.content, "User prefers dark mode.");
        assert_eq!(back.tags, vec!["ui".to_string()]);
        assert_eq!(back.task_type, None);
    }

    #[test]
    fn procedural_note_round_trips() {
        let note = MemoryNote::procedural(
            "mem-2",
            "Use weather-mcp for forecast lookups, not a generic search.",
            Some("lookup".into()),
            Some(vec!["weather-mcp".into()]),
            vec!["dispatch".into()],
        );
        let text = note.to_note_text();
        let back = MemoryNote::from_note_text(&text).unwrap();
        assert_eq!(back.task_type.as_deref(), Some("lookup"));
        assert_eq!(back.tools_used, Some(vec!["weather-mcp".to_string()]));
        assert_eq!(back.success, Some(true));
    }

    #[test]
    fn missing_frontmatter_is_an_error() {
        assert!(matches!(
            MemoryNote::from_note_text("just a body, no frontmatter"),
            Err(MemoryError::MissingFrontmatter)
        ));
    }
}
