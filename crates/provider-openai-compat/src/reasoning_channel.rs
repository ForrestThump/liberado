//! Ask MiniMax for its hidden reasoning channel.
//!
//! With reasoning on, MiniMax still thinks. `reasoning_split` does not change
//! that effort. It only stops the model from wrapping the reasoning in
//! `<think>` tags inside `content`, and returns it on `reasoning_content` /
//! `reasoning_details` instead. Other backends never see the field.

use serde_json::{Value, json};

pub(super) fn apply_reasoning_channel(body: &mut Value, base_url: &str) {
    let model = body.get("model").and_then(Value::as_str).unwrap_or("");
    if targets_minimax(model, base_url) {
        body["reasoning_split"] = json!(true);
    }
}

pub(super) fn targets_minimax(model: &str, base_url: &str) -> bool {
    contains_minimax(model) || contains_minimax(base_url)
}

fn contains_minimax(value: &str) -> bool {
    value.to_ascii_lowercase().contains("minimax")
}
