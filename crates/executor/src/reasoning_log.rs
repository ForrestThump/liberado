//! Per-turn thinking captured beside assistant messages.
//!
//! The provider [`Message`](liberado_provider::Message) is what the next request sends, so
//! thinking is not stored on it. This log is one entry per assistant message the executor
//! appended during the turn, in order. The chat store copies it onto `NewNode` and drops it.

use std::sync::{Arc, Mutex};

pub(crate) fn note(log: &Arc<Mutex<Vec<Option<String>>>>, reasoning: Option<String>) {
    let Ok(mut guard) = log.lock() else {
        return;
    };
    guard.push(blank_to_none(reasoning));
}

pub(crate) fn clear(log: &Arc<Mutex<Vec<Option<String>>>>) {
    if let Ok(mut guard) = log.lock() {
        guard.clear();
    }
}

/// Take the thinking recorded for this executor since the last clear.
pub fn take(executor: &crate::Executor) -> Vec<Option<String>> {
    executor
        .reasoning
        .lock()
        .map(|mut guard| std::mem::take(&mut *guard))
        .unwrap_or_default()
}

fn blank_to_none(reasoning: Option<String>) -> Option<String> {
    reasoning.and_then(|text| {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}
