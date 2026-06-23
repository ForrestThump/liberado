# liberado-provider-deepseek — the concrete inference backend

A thin [`Provider`](../provider/ARCHITECTURE.md) implementation for DeepSeek's OpenAI-compatible
chat API, over `reqwest`. The only crate that talks HTTP to a model.

## Surface

- `DeepSeekProvider` — implements `Provider`. `complete()` translates the normalized
  `CompletionRequest` to the OpenAI request shape, POSTs it, and maps the response back.
- `DeepSeekProvider::from_env()` — reads `DEEPSEEK_API_KEY` (required) and `DEEPSEEK_MODEL`
  (optional). `DEFAULT_BASE_URL = https://api.deepseek.com`.
- Pure mappers `to_openai_request` / `from_openai_response` (unit-testable without the network):
  `ResponseFormat::Json` → `{"type":"json_object"}` (DeepSeek's JSON mode **ignores the schema** —
  the prompt carries the shape); top-level `error` on a 2xx is surfaced as `InvalidRequest`;
  unparseable tool calls are `warn!`-logged rather than silently dropped.

## Design notes

- **`default-tls` (SChannel on Windows)** — deliberately avoids `ring`/`aws-lc` so there are no
  C-toolchain build issues on the homelab/Windows dev box.
- Errors preserve diagnostics: a failed error-body read still yields a useful message rather than
  an empty one.

## Dependencies

- Depends on: `liberado-provider` (the trait + types), `reqwest`, `serde_json`, `tracing`.
- Depended on by: `cli` (the wired backend).

## Tests

Inline `#[cfg(test)]`: request/response mapping, JSON-mode flag, error-body handling. The live
end-to-end smoke test is `#[ignore]` (runs only with a real `DEEPSEEK_API_KEY`).
