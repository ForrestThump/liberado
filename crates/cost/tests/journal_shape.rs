//! The reader and the writer must agree, and something has to check that they do.
//!
//! `JournalEvent` is a hand-written mirror of `liberado_provider::LatencyEvent` — this crate
//! deliberately parses JSONL rather than depending on the provider at runtime. That independence
//! is the point, and it is also the hazard: every field on the reader is `#[serde(default)]`, so a
//! rename on the writer does not fail to parse. It silently yields `None`.
//!
//! The damage that causes is quiet. Rename `cached_prompt_tokens` and the cost tool keeps running,
//! keeps printing money, and reports `cache_hit_rate: n/a` — with every cached token repriced at
//! the full input rate. The total goes *up* and nothing looks broken.
//!
//! §6 of `docs/spec/architecture/failure-modes.md`: two things that should agree, and nothing
//! checks that they do. This is the check.

use liberado_cost::{JournalEvent, load_latency_events_from_str};
use liberado_provider::latency::LatencyEvent;

/// Distinctive values so a field landing in the wrong place is visible, not plausible.
fn writer_event() -> LatencyEvent {
    LatencyEvent {
        ts_ms: 1_754_000_000_123,
        correlation: "chat-delegate-01KZ0JQJ5V359744Y3Q2M5RGXC".into(),
        role: "orchestrator",
        model: "deepseek/deepseek-v4-pro".into(),
        kind: "llm_call",
        wall_ms: 20_531,
        ttft_ms: Some(1_204),
        prompt_tokens: Some(24_455),
        completion_tokens: Some(1_007),
        total_tokens: Some(25_462),
        cached_prompt_tokens: Some(20_736),
        finish: "stop".into(),
        tool_calls: 3,
        streamed: true,
    }
}

fn parse_one(json: &str) -> JournalEvent {
    let mut events = load_latency_events_from_str(json).expect("reader parses a writer record");
    assert_eq!(events.len(), 1, "one line in, one event out");
    events.remove(0)
}

/// Every field this crate prices or joins on must survive a real write → read round trip.
#[test]
fn reader_parses_every_field_the_writer_emits() {
    let written = serde_json::to_string(&writer_event()).expect("writer serializes");
    let read = parse_one(&written);

    assert_eq!(read.ts_ms, 1_754_000_000_123, "ts_ms — turn ordering");
    assert_eq!(
        read.correlation, "chat-delegate-01KZ0JQJ5V359744Y3Q2M5RGXC",
        "correlation — the whole parent join hangs off this"
    );
    assert_eq!(read.role, "orchestrator", "role — per-role rollup");
    assert_eq!(
        read.model, "deepseek/deepseek-v4-pro",
        "model — price lookup"
    );
    assert_eq!(read.kind, "llm_call");
    assert_eq!(read.wall_ms, 20_531);
    assert_eq!(read.prompt_tokens, Some(24_455), "priced at the input rate");
    assert_eq!(
        read.completion_tokens,
        Some(1_007),
        "priced at the output rate"
    );
    assert_eq!(read.total_tokens, Some(25_462));
    assert_eq!(
        read.cached_prompt_tokens,
        Some(20_736),
        "priced at the cached rate — losing this silently inflates every total"
    );
    assert_eq!(read.finish, "stop");
    assert_eq!(read.tool_calls, 3);
    assert!(read.streamed);
}

/// The writer omits absent usage entirely (`skip_serializing_if`). Absent must stay absent —
/// a streaming call that reported nothing is not a call that cost nothing.
#[test]
fn omitted_usage_reads_back_as_absent_not_zero() {
    let mut event = writer_event();
    event.ttft_ms = None;
    event.prompt_tokens = None;
    event.completion_tokens = None;
    event.total_tokens = None;
    event.cached_prompt_tokens = None;

    let written = serde_json::to_string(&event).expect("writer serializes");
    assert!(
        !written.contains("prompt_tokens"),
        "writer is expected to omit absent usage, not emit null: {written}"
    );

    let read = parse_one(&written);
    assert_eq!(read.prompt_tokens, None);
    assert_eq!(read.completion_tokens, None);
    assert_eq!(read.total_tokens, None);
    assert_eq!(
        read.cached_prompt_tokens, None,
        "absent means the backend volunteered nothing; Some(0) would mean caching is on and failing"
    );
    // The fields that are always written still arrive.
    assert_eq!(read.correlation, event.correlation);
    assert_eq!(read.model, event.model);
}
