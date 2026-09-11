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
        if chosen.is_some() {
            return Err(ReviewResultError::Malformed);
        }
        chosen = Some(ReviewResult::parse_success(text, expected_sha)?);
    }
    chosen.ok_or(ReviewResultError::Malformed)
}

/// Extract a result from a raw object or OpenCode `--format json` events.
pub fn parse_opencode_success(
    output: &str,
    expected_sha: &str,
) -> Result<ReviewResult, ReviewResultError> {
    match parse_codex_success(output, expected_sha) {
        Ok(result) => return Ok(result),
        Err(ReviewResultError::StaleSha) => return Err(ReviewResultError::StaleSha),
        Err(ReviewResultError::Malformed) => {}
    }
    let mut chosen: Option<ReviewResult> = None;
    for line in output.lines().filter(|line| !line.trim().is_empty()) {
        let Ok(event) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        for text in opencode_text_fields(&event) {
            match ReviewResult::parse_success(text, expected_sha) {
                Ok(result) if chosen.as_ref() == Some(&result) => {}
                Ok(_) if chosen.is_some() => return Err(ReviewResultError::Malformed),
                Ok(result) => chosen = Some(result),
                Err(ReviewResultError::StaleSha) => return Err(ReviewResultError::StaleSha),
                Err(ReviewResultError::Malformed) => {}
            }
        }
    }
    chosen.ok_or(ReviewResultError::Malformed)
}

fn opencode_text_fields(event: &serde_json::Value) -> Vec<&str> {
    [
        event.get("text"),
        event.get("message"),
        event.pointer("/part/text"),
        event.pointer("/info/text"),
    ]
    .into_iter()
    .filter_map(|value| value.and_then(serde_json::Value::as_str))
    .collect()
}
