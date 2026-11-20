# liberado-provider — the inference narrow waist

Every LLM call in the system — dispatcher classification, the executor's agent loop, subagents —
passes through the [`Provider`] trait. Models and backends are swappable from config and tests inject
a mock (Decision 13 + 16). This crate is intentionally minimal: a normalized request/response
vocabulary and a mock. **It commits to no framework.** The `openai_compat` translation logic is pure;
the only HTTP coupling is the `reqwest` SSE response adapter.

## Surface

- **`Provider`** trait — one method, `complete(CompletionRequest) -> CompletionResponse`, plus
  `model()`. Tool-calling and structured output are expressed through the request/response *types*
  rather than separate methods, which keeps the trait **dyn-compatible** (the system holds
  `Arc<dyn Provider>` and swaps mock vs. real, or different models per role).
- **`complete_json`** — a free function (not a trait method, to preserve dyn-compatibility) that runs
  a request in JSON mode and deserializes the reply into a typed `T`. This is the dispatcher's path
  to a typed `DispatchDecision`. It **re-asks once** on an undecodable or empty reply: structured
  output is near-deterministic, so a malformed reply is usually a transient hiccup, and an
  unattended run gets no second chance (a live evening-debrief cron burned a whole run on one bad
  reply). Deliberately narrow — transport and rate-limit errors are *not* retried here, because
  those have backoff semantics at the caller and silently re-issuing them would hide load problems
  and double spend on a provider already failing. The `Decode` error also carries a 400-char prefix
  of what the model actually said, so a failure is diagnosable after the fact instead of
  unanswerable.
- **Types** (`types.rs`) — `CompletionRequest`/`CompletionResponse`, `Message`/`Role`, `ToolDef`,
  `ToolInvocation`, `ResponseFormat` (`Text`|`Json{schema}`), `FinishReason`, `Usage`. A
  chat-completions-shaped, provider-agnostic narrow waist. `Usage::cached_prompt_tokens` reads
  whatever cache accounting the backend volunteers (DeepSeek's top-level `prompt_cache_hit_tokens`,
  OpenAI's nested `prompt_tokens_details.cached_tokens`) and stays `None` when it volunteers
  nothing — "we cannot see" and "nothing is cached" are different answers, and conflating them is
  how prompt caching looked like an unclaimed cost lever when it had been running at 93–98% all
  along. Recorded in the latency journal so hit rate is a query, not a guess.
- **`MockProvider`** (`mock.rs`) — scriptable test double: hand it a queue of `CompletionResponse`s,
  it pops one per call and records every request, so a scenario can assert on both what the system
  did with a response and what it sent.
- **`openai_compat`** (`openai_compat.rs`) — shared, pure (still no HTTP) translation logic for any
  OpenAI-compatible chat-completions backend: tool-name sanitization, request/response JSON mapping,
  SSE stream-delta accumulation. Added 2026-07-04 (`docs/future-work/archive/hygiene-audit-2026-07-04.md`) after
  `cargo dupes` found `provider-deepseek` and `provider-openrouter` had byte-for-byte identical copies
  of this logic. Both crates are gone: `liberado-provider-openai-compat` now carries DeepSeek,
  OpenRouter, and other OpenAI-compatible backends in one config-driven crate, keeping only the
  per-backend defaults, env-var names, and status-code mapping.

## Dependencies

- Depends on: `liberado-common`, plus `serde`, `serde_json`, `async-trait`, `async-stream`,
  `futures`, `reqwest`, `thiserror`, `tokio`, `tracing`. Concrete backends live in their own crates.
- Depended on by: `liberado-provider-openai-compat` (implements `Provider` on top of `openai_compat`),
  `liberado-dispatcher`, `liberado-executor`, `liberado-orchestrator`, `liberado-mcp` (tool types),
  `liberado-agent-workspace`, `liberado-chat-search`, `liberado-conversation-store`,
  `liberado-cost`, `liberado-scratchpad`, `liberado-session-store`, `liberado-tool-runtime`,
  `liberado-coder-*`, `liberado-dispatch-pack`, `liberado-eval`, `liberado-heuristics-tuner`,
  `liberado-main-agent`, `liberado-acp-bridge`, `liberado-server`, `liberado-test-support`,
  `liberado-telegram-approvals`.

## Design notes

- The two capabilities the model-capability floor cares about — **tool-calling** and **structured
  output** — are the only things the vocabulary must express.
- A `ProviderError::Decode` from `complete_json` is the hook the dispatcher maps into its
  retry/repair/escalate flow.

## Tests

`tests/coverage.rs` + `mock.rs` inline tests (scripting, request recording, `complete_json` decode).
