//! Split from `latency.rs` for module-health boundaries.

use super::*;

fn event(correlation: &str) -> LatencyEvent {
    LatencyEvent {
        ts_ms: 1,
        correlation: correlation.into(),
        role: "face",
        model: "test-model".into(),
        kind: "llm_call",
        wall_ms: 5,
        ttft_ms: None,
        prompt_tokens: None,
        completion_tokens: None,
        total_tokens: None,
        cached_prompt_tokens: None,
        finish: "stop".into(),
        tool_calls: 0,
        streamed: false,
        repeat_calls: None,
    }
}

/// record() must actually hand the event to the writer: the journal file gains one JSON
/// line per recorded call. A no-op recorder would silently empty the daemon's only
/// latency evidence.
#[tokio::test]
async fn recorded_events_land_in_the_jsonl_journal() {
    let dir = tempfile::tempdir().unwrap();
    let recorder = JsonlLatencyRecorder::spawn_at(dir.path().join("latency"));
    recorder.record(event("corr-marker-1"));
    recorder.record(event("corr-marker-2"));

    let path = dir.path().join("latency").join("events.jsonl");
    let mut contents = String::new();
    for _ in 0..50 {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        contents = std::fs::read_to_string(&path).unwrap_or_default();
        if contents.matches("corr-marker").count() == 2 {
            break;
        }
    }
    assert_eq!(
        contents.matches("corr-marker").count(),
        2,
        "both recorded events must reach {path:?}: {contents}"
    );
}
