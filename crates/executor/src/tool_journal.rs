//! Record one latency row per tool runtime call, without argument or result text.
//!
//! The face executor is the loop that hit the budget-bot turn ceiling. Delegated executors
//! built inside the orchestrator, and the coding worker's executor, are separate `Executor::new`
//! sites and do not carry this journal.

use std::sync::Arc;
use std::time::Instant;

use liberado_provider::{LatencyRecorder, ToolCallEvent, ToolInvocation};

use super::ToolRuntime;
use crate::Executor;

pub(super) struct ToolRun {
    pub shown: String,
    pub raw: String,
    pub ok: bool,
}

impl Executor {
    /// Attach the daemon's latency journal. Tool rows share the LLM file.
    #[must_use]
    pub fn with_tool_journal(mut self, recorder: Arc<dyn LatencyRecorder>) -> Self {
        self.tool_journal = Some(recorder);
        self
    }

    /// This executor with a different turn cap. Extra limits stay.
    #[must_use]
    pub fn with_max_turns(mut self, max_turns: u32) -> Self {
        self.budget = self.budget.with_max_turns(max_turns);
        self
    }

    /// The turn cap the loop will use.
    #[must_use]
    pub fn max_turns(&self) -> u32 {
        self.budget.max_turns
    }

    pub(super) async fn run_recorded_tool(
        &self,
        turn: u32,
        runtime: &dyn ToolRuntime,
        call: &ToolInvocation,
    ) -> ToolRun {
        run_and_record(
            self.tool_journal.as_ref(),
            turn,
            runtime,
            call,
            self.spill_dir.as_deref(),
            self.spill_max_bytes,
        )
        .await
    }
}

async fn run_and_record(
    journal: Option<&Arc<dyn LatencyRecorder>>,
    turn: u32,
    runtime: &dyn ToolRuntime,
    call: &ToolInvocation,
    spill_dir: Option<&std::path::Path>,
    spill_max_bytes: usize,
) -> ToolRun {
    let started = Instant::now();
    let (ok, raw) = match runtime.invoke(call).await {
        Ok(content) => (true, content),
        Err(message) => (false, format!("tool error: {message}")),
    };
    let shown = super::run_tool_spill(&raw, spill_dir, spill_max_bytes, &call.id);
    if let Some(journal) = journal {
        journal.record_tool(tool_event(turn, call, &raw, ok, started.elapsed()));
    }
    ToolRun { shown, raw, ok }
}

fn tool_event(
    turn: u32,
    call: &ToolInvocation,
    raw: &str,
    ok: bool,
    elapsed: std::time::Duration,
) -> ToolCallEvent {
    ToolCallEvent {
        ts_ms: unix_ms(),
        kind: "tool_call",
        correlation: liberado_provider::latency::current_correlation(),
        turn,
        tool: call.name.clone(),
        args_bytes: args_len(&call.arguments),
        result_bytes: raw.len() as u64,
        duration_ms: u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
        is_error: !ok,
    }
}

fn args_len(arguments: &serde_json::Value) -> u64 {
    serde_json::to_vec(arguments)
        .map(|bytes| bytes.len() as u64)
        .unwrap_or(0)
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
#[path = "tool_journal_tests.rs"]
mod tests;
