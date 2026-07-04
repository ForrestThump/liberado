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
- The pure mappers (`to_openai_request`/`from_openai_response`/tool-name sanitization/SSE-delta
  accumulation) now live in `liberado_provider::openai_compat`, shared with
  `provider-deepseek` (moved 2026-07-04, see below). This crate only keeps `map_status` (its `402`
  handling is OpenRouter-specific) and the actual HTTP round-trip.

## Design notes

- **Tool-calling/JSON-mode support varies by routed model** — OpenRouter passes the request
  through rather than normalizing per-model capability gaps. Picking a capable model for the task
  is the caller's job (Decision 13's role-tiered model floors apply here too).
- **`402` (insufficient account credits)** is OpenRouter-specific; folded into the same
  `InvalidRequest` bucket as other client-error statuses — no caller branches on "out of credits"
  differently from any other rejected request today.
- **Duplication with `liberado-provider-deepseek` — resolved 2026-07-04.** Both crates speak the
  same OpenAI-compatible wire format, so the translation logic used to be near-identical
  (byte-for-byte, per `cargo dupes` — `docs/roadmap/hygiene-audit-2026-07-04.md`). Now factored into
  `liberado_provider::openai_compat`, shared by both; only `map_status`'s differing status-code set,
  `DEFAULT_BASE_URL`/`DEFAULT_MODEL`, and the env-var names stay per-crate.

## Dependencies

- Depends on: `liberado-provider` (the trait + types + `openai_compat`), `reqwest`, `serde_json`,
  `futures`, `async-stream`.
- Depended on by: `liberado-heuristics-tuner` (the concurrent-scoring backend for prompt tuning).

## Tests

Inline `#[cfg(test)]`: request/response mapping, tool-name sanitization + collision handling,
status-code mapping (including `402`), constructor/env/base-url behavior. The live end-to-end smoke
test is `#[ignore]` (runs only with a real `OPENROUTER_API_KEY`).
