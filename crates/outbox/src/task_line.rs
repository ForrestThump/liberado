//! Shared task-line identity for Done and the completion log.
//!
//! Done matches the stripped description. The log's identity is that same text
//! with whitespace collapsed, unless the line ends in a `^blockid`. The block
//! id then replaces the text, so a reworded habit stays one task.

use crate::keys::block_id;

const METADATA_MARKERS: &[&str] = &[
    " 📅", " ⏳", " 🛫", " ✅", " ❌", " 🔁", " ⏫", " 🔽", " 🔺", " #", " ^",
];

/// Drop Obsidian Tasks markers from a checkbox body. The cut is the same one Done uses.
pub fn strip_task_metadata(body: &str) -> &str {
    let mut end = body.len();
    for marker in METADATA_MARKERS {
        if let Some(idx) = body.find(marker) {
            end = end.min(idx);
        }
    }
    body[..end].trim_end()
}

/// Collapse runs of whitespace to a single space and trim the ends.
pub fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Identity key for one task line.
///
/// A trailing `^blockid` wins and is stored with the caret. Otherwise the key
/// is the stripped description with whitespace collapsed. `None` when the line
/// has no checkbox body, or the description is empty and there is no block id.
pub fn task_identity_key(line: &str) -> Option<String> {
    if let Some(block) = block_id(line) {
        return Some(format!("^{block}"));
    }
    let body = checkbox_body(line)?;
    let collapsed = collapse_whitespace(strip_task_metadata(body).trim());
    if collapsed.is_empty() {
        None
    } else {
        Some(collapsed)
    }
}

/// True when the raw line contains the Obsidian Tasks recurrence marker.
pub fn line_is_recurring(line: &str) -> bool {
    line.contains('🔁')
}

fn checkbox_body(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    let (_, after) = trimmed.split_once(']')?;
    Some(after.trim_start())
}

#[cfg(test)]
#[path = "task_line_tests.rs"]
mod task_line_tests;
