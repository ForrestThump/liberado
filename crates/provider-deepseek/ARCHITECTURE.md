# liberado-provider-deepseek — the concrete inference backend

A thin [`Provider`](../provider/ARCHITECTURE.md) implementation for DeepSeek's OpenAI-compatible
chat API, over `reqwest` — the wired production backend (`liberado-cli`'s dependency). As of
[`liberado-provider-openrouter`](../provider-openrouter/ARCHITECTURE.md), no longer the only crate
that talks HTTP to a model — that one exists to run many models concurrently through one API
(scaffolded ahead of the heuristics tuning engine), not to replace this one.

## Surface

- `DeepSeekProvider` — implements `Provider`. `complete()` translates the normalized
  `CompletionRequest` to the OpenAI request shape, POSTs it, and maps the response back.
- `DeepSeekProvider::from_env()` — reads `DEEPSEEK_API_KEY` (required) and `DEEPSEEK_MODEL`
  (optional). `DEFAULT_BASE_URL = https://api.deepseek.com`.
- The pure mappers (`to_openai_request`/`from_openai_response`/tool-name sanitization/SSE-delta
  accumulation) now live in `liberado_provider::openai_compat`, shared with
  `provider-openrouter` (moved 2026-07-04 — `cargo dupes` found the two crates carried
  byte-for-byte identical copies, `docs/roadmap/hygiene-audit-2026-07-04.md`). This crate only
  keeps `map_status` (DeepSeek doesn't have OpenRouter's extra `402` case) and the actual HTTP
  round-trip. `ResponseFormat::Json` → `{"type":"json_object"}` (DeepSeek's JSON mode **ignores
  the schema** — the prompt carries the shape); top-level `error` on a 2xx is surfaced as
  `InvalidRequest`; unparseable tool calls are `warn!`-logged (in the shared module) rather than
  silently dropped.

## Design notes

- **`default-tls` (SChannel on Windows)** — deliberately avoids `ring`/`aws-lc` so there are no
  C-toolchain build issues on the homelab/Windows dev box.
- Errors preserve diagnostics: a failed error-body read still yields a useful message rather than
  an empty one.

## Dependencies

- Depends on: `liberado-provider` (the trait + types + `openai_compat`), `reqwest`, `serde_json`.
- Depended on by: `cli` (the wired backend).

## Tests

Inline `#[cfg(test)]`: request/response mapping, JSON-mode flag, error-body handling. The live
end-to-end smoke test is `#[ignore]` (runs only with a real `DEEPSEEK_API_KEY`).
