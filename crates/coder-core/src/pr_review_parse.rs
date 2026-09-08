//! Codex output framing for daemon-native pull-request review.

use crate::pr_review::{MAX_RAW_OUTPUT_BYTES, ReviewResult, ReviewResultError};

/// Extract a result from either the legacy plain object or Codex `--json` JSONL events.
///
/// TODO(slice-3 residual): replace the synthetic JSONL fixture with a live `codex exec review
/// --json` capture once quota allows. Until then, keep fail-closed JSONL extraction and do not
/// treat the Slice-2 single-object stdout fixture as the only production framing.
pub fn parse_codex_success(
    output: &str,
    expected_sha: &str,
) -> Result<ReviewResult, ReviewResultError> {
    if output.len() > MAX_RAW_OUTPUT_BYTES {
        return Err(ReviewResultError::Malformed);
    }
    match ReviewResult::parse_success(output, expected_sha) {
        Ok(result) => return Ok(result),
        Err(ReviewResultError::StaleSha) => return Err(ReviewResultError::StaleSha),
        Err(ReviewResultError::Malformed) => {}
    }
    let mut chosen: Option<ReviewResult> = None;
    for line in output.lines().filter(|line| !line.trim().is_empty()) {
        let event: serde_json::Value =
            serde_json::from_str(line).map_err(|_| ReviewResultError::Malformed)?;
        let text = event
            .get("type")
            .and_then(|value| value.as_str())
            .filter(|kind| *kind == "item.completed")
            .and_then(|_| event.get("item"))
            .filter(|item| {
                item.get("type").and_then(|value| value.as_str()) == Some("agent_message")
            })
            .and_then(|item| item.get("text"))
            .and_then(|value| value.as_str());
        let Some(text) = text else { continue };
        let Ok(result) = ReviewResult::parse_success(text, expected_sha) else {
            continue;
        };
        if chosen.as_ref().is_some_and(|old| old != &result) {
            return Err(ReviewResultError::Malformed);
        }
        chosen = Some(result);
    }
    chosen.ok_or(ReviewResultError::Malformed)
}
