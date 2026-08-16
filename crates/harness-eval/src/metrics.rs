//! Roll-up of per-harness run metrics into `report.json` (fairness item F4).
//!
//! The durable system previously recorded only exit codes and commit hashes in `report.json`; wall
//! clock, turns, and tokens lived in artifacts and had to be re-dug by hand per run, where analysis
//! errors breed. This module parses them once, from the same artifacts the comparison already
//! preserves, so every scoreboard reads them off the report.

use std::fs;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::Deserialize;

/// Parsed, normalized metrics for a single harness run.
#[derive(Debug, Clone, Default)]
pub struct HarnessMetrics {
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub duration_secs: Option<f64>,
    pub turns_used: Option<u32>,
    pub tokens_in: Option<u64>,
    pub tokens_out: Option<u64>,
}

impl HarnessMetrics {
    /// Parse every metric available for the given harness from its artifact directory.
    ///
    /// Missing or unparseable artifacts yield `None` for the affected fields rather than failing the
    /// whole report; a partial comparison is still a comparison, and the field's absence is itself
    /// information. The `harness_id` selects the transcript parser because the two harnesses write
    /// different shapes (`coder-traces` JSON for Liberado, `session.jsonl` for pi).
    pub fn collect(harness_id: &str, harness_dir: &Path) -> Self {
        let mut metrics = Self::from_run_status(&harness_dir.join("run-status.txt"));
        let (turns, tokens_in, tokens_out) = match harness_id {
            "liberado" => parse_liberado_traces(harness_dir),
            "pi" => parse_pi_sessions(harness_dir),
            _ => (None, None, None),
        };
        metrics.turns_used = turns;
        metrics.tokens_in = tokens_in;
        metrics.tokens_out = tokens_out;
        metrics
    }

    fn from_run_status(path: &Path) -> Self {
        let Ok(text) = fs::read_to_string(path) else {
            return Self::default();
        };
        let mut started_at = None;
        let mut finished_at = None;
        for line in text.lines() {
            if let Some(value) = line.strip_prefix("started=") {
                started_at = DateTime::parse_from_rfc3339(value)
                    .ok()
                    .map(|t| t.with_timezone(&Utc));
            } else if let Some(value) = line.strip_prefix("finished=") {
                finished_at = DateTime::parse_from_rfc3339(value)
                    .ok()
                    .map(|t| t.with_timezone(&Utc));
            }
        }
        let duration_secs = match (started_at, finished_at) {
            (Some(start), Some(finish)) => {
                let secs = (finish - start).num_microseconds().unwrap_or(0) as f64 / 1_000_000.0;
                Some(secs.max(0.0))
            }
            _ => None,
        };
        Self {
            started_at,
            finished_at,
            duration_secs,
            turns_used: None,
            tokens_in: None,
            tokens_out: None,
        }
    }
}

/// Liberado writes one `CoderTrace` JSON per session under `traces/`. Each event is an externally
/// tagged enum; we only need `ModelTurnFinished`, which carries `prompt_tokens` /
/// `completion_tokens`. Turns and tokens are summed across all sessions (the pack may fan out).
fn parse_liberado_traces(harness_dir: &Path) -> (Option<u32>, Option<u64>, Option<u64>) {
    let traces_dir = harness_dir.join("traces");
    if !traces_dir.is_dir() {
        return (None, None, None);
    }
    let mut turns = 0u32;
    let mut in_tokens = 0u64;
    let mut out_tokens = 0u64;
    let mut found = false;
    for entry in fs::read_dir(&traces_dir).into_iter().flatten().flatten() {
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let Ok(text) = fs::read_to_string(entry.path()) else {
            continue;
        };
        let Ok(trace) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let Some(events) = trace.get("events").and_then(|e| e.as_array()) else {
            continue;
        };
        for event in events {
            let Some((name, body)) = event.as_object().and_then(|o| o.iter().next()) else {
                continue;
            };
            if name != "ModelTurnFinished" {
                continue;
            }
            found = true;
            turns += 1;
            if let Some(value) = body.get("prompt_tokens").and_then(|v| v.as_u64()) {
                in_tokens += value;
            }
            if let Some(value) = body.get("completion_tokens").and_then(|v| v.as_u64()) {
                out_tokens += value;
            }
        }
    }
    if !found {
        (None, None, None)
    } else {
        (Some(turns), Some(in_tokens), Some(out_tokens))
    }
}

#[derive(Debug, Deserialize)]
struct PiUsage {
    #[serde(default)]
    prompt_tokens: Option<u64>,
    #[serde(default)]
    completion_tokens: Option<u64>,
    #[serde(default)]
    input_tokens: Option<u64>,
    #[serde(default)]
    output_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct PiRecord {
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    usage: Option<PiUsage>,
}

/// pi writes `--mode json` session lines under `sessions/`. The exact schema is owned by the pi
/// binary, so this parser is deliberately tolerant: any line that carries a usage block contributes
/// its tokens, and every assistant-style line counts as a turn. Unknown shapes yield `None` rather
/// than corrupting the report.
fn parse_pi_sessions(harness_dir: &Path) -> (Option<u32>, Option<u64>, Option<u64>) {
    let sessions_dir = harness_dir.join("sessions");
    if !sessions_dir.is_dir() {
        return (None, None, None);
    }
    let mut turns = 0u32;
    let mut in_tokens = 0u64;
    let mut out_tokens = 0u64;
    let mut found = false;
    for entry in fs::read_dir(&sessions_dir).into_iter().flatten().flatten() {
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let Ok(text) = fs::read_to_string(entry.path()) else {
            continue;
        };
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Ok(record) = serde_json::from_str::<PiRecord>(line) else {
                continue;
            };
            found = true;
            if record.role.as_deref() == Some("assistant") {
                turns += 1;
            }
            if let Some(usage) = record.usage {
                in_tokens += usage.prompt_tokens.or(usage.input_tokens).unwrap_or(0);
                out_tokens += usage.completion_tokens.or(usage.output_tokens).unwrap_or(0);
            }
        }
    }
    if !found {
        (None, None, None)
    } else {
        (
            if turns > 0 { Some(turns) } else { None },
            if in_tokens > 0 { Some(in_tokens) } else { None },
            if out_tokens > 0 {
                Some(out_tokens)
            } else {
                None
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn run_status_parses_start_finish_and_duration() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("run-status.txt");
        let start = Utc.with_ymd_and_hms(2026, 8, 16, 10, 0, 0).unwrap();
        let finish = Utc.with_ymd_and_hms(2026, 8, 16, 10, 0, 30).unwrap();
        fs::write(
            &path,
            format!(
                "started={}\nfinished={}\nexit=0\n",
                start.to_rfc3339(),
                finish.to_rfc3339()
            ),
        )
        .unwrap();
        let metrics = HarnessMetrics::from_run_status(&path);
        assert_eq!(metrics.started_at, Some(start));
        assert_eq!(metrics.finished_at, Some(finish));
        assert_eq!(metrics.duration_secs, Some(30.0));
    }

    #[test]
    fn liberado_trace_sums_turns_and_tokens_across_sessions() {
        let temp = tempfile::tempdir().unwrap();
        let traces = temp.path().join("traces");
        fs::create_dir_all(&traces).unwrap();
        let trace = serde_json::json!({
            "session_id": "s1",
            "events": [
                {"ModelTurnFinished": {"turn": 1, "prompt_tokens": 10, "completion_tokens": 2}},
                {"ModelRequestSent": {"turn": 2, "tools_offered": []}},
                {"ModelTurnFinished": {"turn": 2, "prompt_tokens": 5, "completion_tokens": 3}},
                {"ToolFinished": {"name": "read_file", "ok": true}}
            ]
        });
        fs::write(
            traces.join("s1.json"),
            serde_json::to_string_pretty(&trace).unwrap(),
        )
        .unwrap();
        let (turns, tokens_in, tokens_out) = parse_liberado_traces(temp.path());
        assert_eq!(turns, Some(2));
        assert_eq!(tokens_in, Some(15));
        assert_eq!(tokens_out, Some(5));
    }

    #[test]
    fn pi_session_counts_assistant_turns_and_tokens() {
        let temp = tempfile::tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        fs::create_dir_all(&sessions).unwrap();
        let mut content = String::new();
        content.push_str("{\"role\":\"user\",\"content\":\"do it\"}\n");
        content.push_str(
            "{\"role\":\"assistant\",\"usage\":{\"prompt_tokens\":100,\"completion_tokens\":20}}\n",
        );
        content.push_str(
            "{\"role\":\"assistant\",\"usage\":{\"input_tokens\":50,\"output_tokens\":5}}\n",
        );
        fs::write(sessions.join("s1.jsonl"), content).unwrap();
        let (turns, tokens_in, tokens_out) = parse_pi_sessions(temp.path());
        assert_eq!(turns, Some(2));
        assert_eq!(tokens_in, Some(150));
        assert_eq!(tokens_out, Some(25));
    }

    #[test]
    fn missing_artifacts_yield_none() {
        let temp = tempfile::tempdir().unwrap();
        let metrics = HarnessMetrics::collect("liberado", temp.path());
        assert_eq!(metrics.started_at, None);
        assert_eq!(metrics.turns_used, None);
        assert_eq!(metrics.tokens_in, None);
    }
}
