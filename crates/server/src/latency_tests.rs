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

#[tokio::test]
async fn tool_rows_land_beside_llm_rows_and_split_by_kind() {
    let dir = tempfile::tempdir().unwrap();
    let recorder = JsonlLatencyRecorder::spawn_at(dir.path().join("latency"));
    recorder.record(event("corr-llm"));
    recorder.record_tool(liberado_provider::ToolCallEvent {
        ts_ms: 2,
        kind: "tool_call",
        correlation: "corr-llm".into(),
        turn: 3,
        tool: "list_loans".into(),
        args_bytes: 11,
        result_bytes: 40,
        duration_ms: 7,
        is_error: true,
    });

    let path = dir.path().join("latency").join("events.jsonl");
    let mut contents = String::new();
    for _ in 0..50 {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        contents = std::fs::read_to_string(&path).unwrap_or_default();
        if contents.contains("list_loans") && contents.contains("corr-llm") {
            break;
        }
    }
    assert!(!contents.contains("arguments"), "{contents}");
    let llm = liberado_cost::load_latency_events_from_str(&contents).expect("llm");
    let tools = liberado_cost::load_tool_call_events_from_str(&contents).expect("tools");
    assert_eq!(llm.len(), 1);
    assert_eq!(llm[0].correlation, "corr-llm");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].tool, "list_loans");
    assert_eq!(tools[0].turn, 3);
    assert!(tools[0].is_error);
    assert_eq!(tools[0].args_bytes, 11);
    assert_eq!(tools[0].result_bytes, 40);
    assert_eq!(tools[0].duration_ms, 7);
}
