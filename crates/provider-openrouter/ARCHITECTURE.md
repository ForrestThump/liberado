# liberado-provider-openrouter — the concurrent-eval inference backend

A thin [`Provider`](../provider/ARCHITECTURE.md) implementation for
[OpenRouter](https://openrouter.ai)'s OpenAI-compatible chat API, over `reqwest`. OpenRouter fronts
many models behind one endpoint/key — the reason to reach for it is running many candidate
evaluations concurrently (the planned heuristics tuning engine,
`docs/roadmap/heuristics-tuning-engine-plan.md`) without a single upstream provider's per-key rate
limits becoming the bottleneck.

## Surface

- `OpenRouterProvider` — implements `Provider`. `complete()`/`complete_stream()` translate the
  normalized `CompletionRequest` to the OpenAI request shape, POST it, and map the response back —
  identical wire format to `liberado-provider-deepseek`.
- `OpenRouterProvider::from_env()` — reads `OPENROUTER_API_KEY` (required) and `OPENROUTER_MODEL`
  (optional, an OpenRouter model slug like `"anthropic/claude-3.5-haiku"`).
  `DEFAULT_MODEL = "openai/gpt-4o-mini"`, `DEFAULT_BASE_URL = https://openrouter.ai/api/v1`.
- Pure mappers `to_openai_request` / `from_openai_response` (unit-testable without the network),
  same shape as the DeepSeek crate's.

## Design notes

- **Tool-calling/JSON-mode support varies by routed model** — OpenRouter passes the request
  through rather than normalizing per-model capability gaps. Picking a capable model for the task
  is the caller's job (Decision 13's role-tiered model floors apply here too).
- **`402` (insufficient account credits)** is OpenRouter-specific; folded into the same
  `InvalidRequest` bucket as other client-error statuses — no caller branches on "out of credits"
  differently from any other rejected request today.
- **Known, deliberate duplication with `liberado-provider-deepseek`**: both crates speak the same
  OpenAI-compatible wire format, so the translation logic (tool-name sanitization, request/response
  mapping, SSE assembly) is near-identical. Not yet factored into a shared crate — see this crate's
  `lib.rs` module doc comment for the reasoning and the trigger condition for when to revisit.

## Dependencies

- Depends on: `liberado-provider` (the trait + types), `reqwest`, `serde_json`, `tracing`,
  `futures`, `async-stream`.
- Depended on by: nothing yet (scaffolded ahead of the heuristics tuning engine; not wired into any
  binary today).

## Tests

Inline `#[cfg(test)]`: request/response mapping, tool-name sanitization + collision handling,
status-code mapping (including `402`), constructor/env/base-url behavior. The live end-to-end smoke
test is `#[ignore]` (runs only with a real `OPENROUTER_API_KEY`).
