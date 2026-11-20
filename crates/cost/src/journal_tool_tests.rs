//! A tool row must not make the status tail throw the journal away.

use super::{load_latency_events_tail, load_tool_call_events_from_str};

#[test]
fn a_tool_row_does_not_blank_the_status_tail() {
    let dir = tempfile::tempdir().expect("dir");
    let path = dir.path().join("events.jsonl");
    let tool = r#"{"ts_ms":1,"kind":"tool_call","correlation":"s","turn":1,"tool":"upcoming","args_bytes":2,"result_bytes":9,"duration_ms":4,"is_error":false}"#;
    let llm = r#"{"ts_ms":2,"correlation":"s","role":"face","model":"m","kind":"llm_call","wall_ms":3,"finish":"stop","tool_calls":1,"streamed":true}"#;
    std::fs::write(&path, format!("{tool}\n{llm}\n")).expect("write");

    let tail = load_latency_events_tail(&path, 64 * 1024).expect("tail parses");
    assert_eq!(tail.len(), 1);
    assert_eq!(tail[0].kind, "llm_call");
    assert_eq!(tail[0].role, "face");
    let tools = load_tool_call_events_from_str(&std::fs::read_to_string(&path).expect("read"))
        .expect("tool rows");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].tool, "upcoming");
    assert_eq!(tools[0].result_bytes, 9);
}

#[test]
fn a_broken_llm_row_still_blanks_the_tail() {
    let dir = tempfile::tempdir().expect("dir");
    let path = dir.path().join("events.jsonl");
    std::fs::write(&path, "{\"kind\":\"llm_call\"}\n").expect("write");
    assert!(load_latency_events_tail(&path, 64 * 1024).is_none());
}
