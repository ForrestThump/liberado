//! Native harness streams mapped into Liberado's publish result.

use crate::pr_review::{
    MAX_RAW_OUTPUT_BYTES, REVIEW_SCHEMA_VERSION, ReviewResult, ReviewResultError,
};

/// Extract a result from either the legacy plain object or Codex `--json` JSONL events.
pub fn parse_codex_success(
    output: &str,
    expected_sha: &str,
) -> Result<ReviewResult, ReviewResultError> {
    parse_native_stream(output, expected_sha, codex_text_fields)
}

/// Map OpenCode `--format json` output into Liberado's publish result.
pub fn parse_opencode_success(
    output: &str,
    expected_sha: &str,
) -> Result<ReviewResult, ReviewResultError> {
    parse_native_stream(output, expected_sha, opencode_text_fields)
}

fn parse_native_stream(
    output: &str,
    expected_sha: &str,
    texts: fn(&serde_json::Value) -> Vec<&str>,
) -> Result<ReviewResult, ReviewResultError> {
    if output.len() > MAX_RAW_OUTPUT_BYTES {
        return Err(ReviewResultError::Malformed);
    }
    match ReviewResult::parse_success(output, expected_sha) {
        Ok(result) => return Ok(result),
        Err(ReviewResultError::StaleSha) => return Err(ReviewResultError::StaleSha),
        Err(ReviewResultError::Malformed) => {}
    }
    let mut structured: Option<ReviewResult> = None;
    let mut last_text: Option<String> = None;
    for line in output.lines().filter(|line| !line.trim().is_empty()) {
        let Ok(event) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        for text in texts(&event) {
            if !text.trim().is_empty() {
                last_text = Some(text.to_string());
            }
            match ReviewResult::parse_success(text, expected_sha) {
                Ok(result) if structured.as_ref() == Some(&result) => {}
                Ok(_) if structured.is_some() => return Err(ReviewResultError::Malformed),
                Ok(result) => structured = Some(result),
                Err(ReviewResultError::StaleSha) => return Err(ReviewResultError::StaleSha),
                Err(ReviewResultError::Malformed) => {}
            }
        }
    }
    if let Some(result) = structured {
        return Ok(result);
    }
    let Some(summary) = last_text
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
    else {
        return Err(ReviewResultError::Malformed);
    };
    wrap_native_summary(&summary, expected_sha)
}

/// Map a print/headless CLI review (Cursor ask, Grok `--single`) into the publish result.
///
/// JSON objects and JSONL text fields are kept when they already match the schema. Otherwise the
/// trimmed stdout is the summary.
pub fn parse_plain_success(
    output: &str,
    expected_sha: &str,
) -> Result<ReviewResult, ReviewResultError> {
    match parse_native_stream(output, expected_sha, opencode_text_fields) {
        Ok(result) => Ok(result),
        Err(ReviewResultError::StaleSha) => Err(ReviewResultError::StaleSha),
        Err(ReviewResultError::Malformed) => wrap_native_summary(output, expected_sha),
    }
}

fn wrap_native_summary(text: &str, expected_sha: &str) -> Result<ReviewResult, ReviewResultError> {
    if text.len() > MAX_RAW_OUTPUT_BYTES {
        return Err(ReviewResultError::Malformed);
    }
    let summary = text.trim();
    if summary.is_empty() {
        return Err(ReviewResultError::Malformed);
    }
    Ok(ReviewResult {
        schema: REVIEW_SCHEMA_VERSION.into(),
        reviewed_sha: expected_sha.into(),
        summary: summary.into(),
        findings: Vec::new(),
    })
}

fn codex_text_fields(event: &serde_json::Value) -> Vec<&str> {
    event
        .get("type")
        .and_then(|value| value.as_str())
        .filter(|kind| *kind == "item.completed")
        .and_then(|_| event.get("item"))
        .filter(|item| item.get("type").and_then(|value| value.as_str()) == Some("agent_message"))
        .and_then(|item| item.get("text"))
        .and_then(|value| value.as_str())
        .map(|text| vec![text])
        .unwrap_or_default()
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
