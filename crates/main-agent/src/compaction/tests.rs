//! Survivor tests for compaction's token estimator — exact arithmetic, not just monotonicity.

use super::*;
use liberado_provider::ToolInvocation;

/// `message_chars` must count content + (tool-call name + arguments JSON) + tool-result id.
///
/// The three survivors here were arithmetic swaps (`+=`→`*=`, `+`→`*`, `+=`→`-=`) that every
/// relative assertion ("with a call > without") survives; only an exact value pins the formula.
/// 2 content + 4 name + 2 args + 3 id = 11 chars → ceil(11/4 × 1.3) = ceil(3.575) = 4.
#[test]
fn estimate_tokens_counts_every_component_exactly() {
    let msg = Message {
        role: Role::Assistant,
        content: "ab".into(),
        tool_calls: vec![ToolInvocation::new("t1", "abcd", serde_json::json!({}))],
        tool_call_id: Some("xyz".into()),
    };
    assert_eq!(estimate_tokens(&[msg]), 4);
}
