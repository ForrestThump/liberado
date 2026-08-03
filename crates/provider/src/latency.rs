//! Latency instrumentation at the inference chokepoint.
//!
//! Every LLM roundtrip in Liberado goes through [`Provider::complete`] / [`complete_stream`], so
//! wrapping the shared provider in [`MeteredProvider`] captures the whole face → dispatcher →
//! orchestrator → face hop chain from one place — one [`LatencyEvent`] per call: role, model, wall
//! time, time-to-first-token (streaming), and token usage.
//!
//! The **role** is fixed on each wrapped provider (the per-role providers the daemon builds from
//! config carry it), so it needs no plumbing. The **correlation id** rides on a tokio task-local set
//! at the hop seams ([`with_correlation`]) — the chat turn keys the session id, the dispatch pack
//! keys the dispatch `correlation_id` — so face turns and the work they trigger can be joined.
//!
//! See `docs/roadmap/latency-and-routing-observability-plan.md` (§4).

use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use futures::StreamExt;
use serde::Serialize;

use crate::error::ProviderResult;
use crate::provider::{CompletionStream, Provider};
use crate::types::{CompletionRequest, CompletionResponse, StreamItem};

/// Which agent made an inference call. Named `AgentRole` to avoid colliding with the message-level
/// [`crate::Role`] (system/user/assistant).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentRole {
    Face,
    Dispatcher,
    Orchestrator,
    /// Delegated subagent work — journaled separately from the orchestrator's own direct execution
    /// so delegation vs. doing-it-directly is measurable (see `docs/future-work/
    /// parallel-deliverables-2026-08-round-3.md` §2).
    Subagent,
    Unknown,
}

impl AgentRole {
    pub fn as_str(self) -> &'static str {
        match self {
            AgentRole::Face => "face",
            AgentRole::Dispatcher => "dispatcher",
            AgentRole::Orchestrator => "orchestrator",
            AgentRole::Subagent => "subagent",
            AgentRole::Unknown => "unknown",
        }
    }
}

tokio::task_local! {
    static CORRELATION: String;
    static REPEAT_CALLS: std::cell::Cell<usize>;
}

/// Run `fut` with `correlation` on the task-local context. All provider calls made *within the same
/// task* inherit it. Note: `tokio::spawn`ed child tasks do **not** inherit — re-wrap inside a spawned
/// future if it makes provider calls (e.g. parallel subagents).
pub async fn with_correlation<F, T>(correlation: impl Into<String>, fut: F) -> T
where
    F: std::future::Future<Output = T>,
{
    CORRELATION.scope(correlation.into(), fut).await
}

/// Run `fut` with `repeat_calls` on the task-local context so the MeteredProvider stamps it onto
/// the next [`LatencyEvent`]. Mirrors [`with_correlation`] in shape.
pub async fn with_repeat_calls<F, T>(count: usize, fut: F) -> T
where
    F: std::future::Future<Output = T>,
{
    use std::cell::Cell;
    REPEAT_CALLS.scope(Cell::new(count), fut).await
}

/// The correlation id on the current task-local context, or `"-"` outside any [`with_correlation`].
pub fn current_correlation() -> String {
    CORRELATION
        .try_with(|c| c.clone())
        .unwrap_or_else(|_| "-".into())
}

/// One recorded inference call. Serialized as a JSONL line by the daemon's recorder.
#[derive(Debug, Clone, Serialize)]
pub struct LatencyEvent {
    /// Wall-clock time the record was made (epoch milliseconds).
    pub ts_ms: u64,
    /// Joins face turns to dispatch work (chat session id, or the dispatch `correlation_id`).
    pub correlation: String,
    /// Which agent made the call.
    pub role: &'static str,
    /// Model id the provider reported.
    pub model: String,
    /// Always `"llm_call"` for now; leaves room for `"stage"` records later.
    pub kind: &'static str,
    /// Total wall time of the call (request → response, or request → final `Done` for streams).
    pub wall_ms: u64,
    /// Time to first streamed token (streaming calls only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttft_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u32>,
    /// Prompt tokens the provider served from cache, when it reports them at all.
    ///
    /// Recorded so cache hit rate is a query over this journal rather than a guess. Absent means
    /// the backend volunteered nothing — distinct from a reported zero, which would mean caching is
    /// available and simply not working.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached_prompt_tokens: Option<u32>,
    /// Finish reason (or `"error"` if the call failed).
    pub finish: String,
    /// Number of tool calls the model requested this turn.
    pub tool_calls: usize,
    /// Whether this went through the streaming path (`complete_stream`).
    pub streamed: bool,
    /// Byte-exact repeated tool calls the executor tallied up to this point in the run.
    /// Absent for providers that don't carry the counter (e.g. dispatcher calls, or pre-PR #44).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repeat_calls: Option<usize>,
}

/// Sink for [`LatencyEvent`]s. The daemon supplies a JSONL-appending implementation; tests and
/// non-daemon callers can use [`NoopRecorder`].
pub trait LatencyRecorder: Send + Sync {
    fn record(&self, event: LatencyEvent);
}

/// Drops every event — the default when no journal is wired.
pub struct NoopRecorder;
impl LatencyRecorder for NoopRecorder {
    fn record(&self, _event: LatencyEvent) {}
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// A [`Provider`] decorator that records one [`LatencyEvent`] per call. The **role** is bound at
/// construction — build one wrapped instance per [`AgentRole`] (the daemon's per-role providers do
/// exactly that) — while only the **correlation id** rides the task-local [`with_correlation`]
/// scope at each seam. No role scope exists, so two roles can never share one instance.
pub struct MeteredProvider {
    inner: Arc<dyn Provider>,
    role: AgentRole,
    recorder: Arc<dyn LatencyRecorder>,
}

impl MeteredProvider {
    pub fn new(
        inner: Arc<dyn Provider>,
        role: AgentRole,
        recorder: Arc<dyn LatencyRecorder>,
    ) -> Self {
        Self {
            inner,
            role,
            recorder,
        }
    }

    /// Convenience: wrap and erase to `Arc<dyn Provider>` so it drops into existing wiring.
    pub fn wrap(
        inner: Arc<dyn Provider>,
        role: AgentRole,
        recorder: Arc<dyn LatencyRecorder>,
    ) -> Arc<dyn Provider> {
        Arc::new(Self::new(inner, role, recorder))
    }
}

#[async_trait]
impl Provider for MeteredProvider {
    fn model(&self) -> String {
        self.inner.model()
    }

    fn set_model(&self, model: String) {
        self.inner.set_model(model);
    }

    async fn list_models(&self) -> ProviderResult<Vec<String>> {
        self.inner.list_models().await
    }

    async fn complete(&self, request: CompletionRequest) -> ProviderResult<CompletionResponse> {
        let role = self.role;
        let correlation = current_correlation();
        let model = self.inner.model();
        let start = Instant::now();
        let result = self.inner.complete(request).await;
        let wall_ms = start.elapsed().as_millis() as u64;

        let (finish, tool_calls, usage) = match &result {
            Ok(r) => (
                format!("{:?}", r.finish_reason),
                r.tool_calls.len(),
                r.usage,
            ),
            Err(_) => ("error".to_string(), 0, None),
        };
        record(
            &self.recorder,
            LatencyEvent {
                ts_ms: now_ms(),
                correlation,
                role: role.as_str(),
                model,
                kind: "llm_call",
                wall_ms,
                ttft_ms: None,
                prompt_tokens: usage.map(|u| u.prompt_tokens),
                completion_tokens: usage.map(|u| u.completion_tokens),
                total_tokens: usage.map(|u| u.total_tokens),
                cached_prompt_tokens: usage.and_then(|u| u.cached_prompt_tokens),
                finish,
                tool_calls,
                streamed: false,
                repeat_calls: REPEAT_CALLS
                    .try_with(|c| c.get())
                    .ok()
                    .and_then(|v| if v > 0 { Some(v) } else { None }),
            },
        );
        result
    }

    async fn complete_stream(
        &self,
        request: CompletionRequest,
    ) -> ProviderResult<CompletionStream> {
        let role = self.role;
        let correlation = current_correlation();
        let model = self.inner.model();
        let recorder = self.recorder.clone();
        let start = Instant::now();
        let inner = self.inner.complete_stream(request).await?;

        // FnMut closure over the sequentially-polled stream: capture TTFT on the first token, and
        // record the full event exactly once when the assembled `Done` arrives.
        let mut ttft_ms: Option<u64> = None;
        let mut recorded = false;
        let mapped = inner.map(move |item| {
            match &item {
                Ok(StreamItem::Token(_)) if ttft_ms.is_none() => {
                    ttft_ms = Some(start.elapsed().as_millis() as u64);
                }
                Ok(StreamItem::Done(resp)) if !recorded => {
                    recorded = true;
                    let usage = resp.usage;
                    record(
                        &recorder,
                        LatencyEvent {
                            ts_ms: now_ms(),
                            correlation: correlation.clone(),
                            role: role.as_str(),
                            model: model.clone(),
                            kind: "llm_call",
                            wall_ms: start.elapsed().as_millis() as u64,
                            ttft_ms,
                            prompt_tokens: usage.map(|u| u.prompt_tokens),
                            completion_tokens: usage.map(|u| u.completion_tokens),
                            total_tokens: usage.map(|u| u.total_tokens),
                            cached_prompt_tokens: usage.and_then(|u| u.cached_prompt_tokens),
                            finish: format!("{:?}", resp.finish_reason),
                            tool_calls: resp.tool_calls.len(),
                            streamed: true,
                            repeat_calls: REPEAT_CALLS
                                .try_with(|c| c.get())
                                .ok()
                                .and_then(|v| if v > 0 { Some(v) } else { None }),
                        },
                    );
                }
                _ => {}
            }
            item
        });
        Ok(Box::pin(mapped))
    }
}

/// Record to the sink and echo a debug line for live tailing (`RUST_LOG=liberado::latency=debug`).
fn record(recorder: &Arc<dyn LatencyRecorder>, event: LatencyEvent) {
    tracing::debug!(
        target: "liberado::latency",
        role = event.role,
        model = %event.model,
        wall_ms = event.wall_ms,
        ttft_ms = ?event.ttft_ms,
        total_tokens = ?event.total_tokens,
        correlation = %event.correlation,
        "llm_call"
    );
    recorder.record(event);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MockProvider;
    use crate::types::{CompletionResponse, Message};
    use std::sync::Mutex;

    #[derive(Default)]
    struct CapturingRecorder {
        events: Mutex<Vec<LatencyEvent>>,
    }
    impl LatencyRecorder for CapturingRecorder {
        fn record(&self, event: LatencyEvent) {
            self.events.lock().unwrap().push(event);
        }
    }

    #[tokio::test]
    async fn records_fixed_role_and_correlation_from_scope() {
        let rec = Arc::new(CapturingRecorder::default());
        let inner: Arc<dyn Provider> = Arc::new(MockProvider::with_script(
            "hi",
            [CompletionResponse::text("ok")],
        ));
        let metered = MeteredProvider::new(inner, AgentRole::Dispatcher, rec.clone());

        with_correlation("cid-123", async {
            metered
                .complete(CompletionRequest::new(vec![Message::user("go")]))
                .await
                .unwrap();
        })
        .await;

        let events = rec.events.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].role, "dispatcher");
        assert_eq!(events[0].correlation, "cid-123");
        assert_eq!(events[0].kind, "llm_call");
    }

    #[tokio::test]
    async fn correlation_defaults_to_dash_outside_a_scope() {
        let rec = Arc::new(CapturingRecorder::default());
        let inner: Arc<dyn Provider> = Arc::new(MockProvider::with_script(
            "hi",
            [CompletionResponse::text("ok")],
        ));
        let metered = MeteredProvider::new(inner, AgentRole::Face, rec.clone());

        metered
            .complete(CompletionRequest::new(vec![Message::user("go")]))
            .await
            .unwrap();

        let events = rec.events.lock().unwrap();
        assert_eq!(events[0].role, "face");
        assert_eq!(events[0].correlation, "-");
    }

    #[tokio::test]
    async fn metered_provider_forwards_model_getter_and_setter() {
        let inner: Arc<dyn Provider> = Arc::new(MockProvider::new("original"));
        let metered =
            MeteredProvider::new(inner.clone(), AgentRole::Unknown, Arc::new(NoopRecorder));

        assert_eq!(metered.model(), "original");

        metered.set_model("updated".into());
        assert_eq!(metered.model(), "updated");
    }

    #[tokio::test]
    async fn complete_stream_records_ttft_and_final_event() {
        use futures::StreamExt;

        let rec = Arc::new(CapturingRecorder::default());
        let inner: Arc<dyn Provider> = Arc::new(MockProvider::with_script(
            "m",
            [CompletionResponse::text("hello world")],
        ));
        let metered = MeteredProvider::new(inner, AgentRole::Orchestrator, rec.clone());

        let stream = metered
            .complete_stream(CompletionRequest::new(vec![Message::user("go")]))
            .await
            .unwrap();
        let _items: Vec<_> = stream.collect().await;

        let events = rec.events.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert!(
            events[0].ttft_ms.is_some(),
            "streamed call should record TTFT"
        );
        assert!(events[0].streamed);
        assert_eq!(events[0].role, "orchestrator");
    }
}
