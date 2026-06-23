# liberado-provider — the inference narrow waist

Every LLM call in the system — dispatcher classification, the executor's agent loop, subagents —
passes through the [`Provider`] trait. Models and backends are swappable from config and tests inject
a mock (Decision 13 + 16). This crate is intentionally minimal: a normalized request/response
vocabulary and a mock. **It pulls in no HTTP stack and commits to no framework.**

## Surface

- **`Provider`** trait — one method, `complete(CompletionRequest) -> CompletionResponse`, plus
  `model()`. Tool-calling and structured output are expressed through the request/response *types*
  rather than separate methods, which keeps the trait **dyn-compatible** (the system holds
  `Arc<dyn Provider>` and swaps mock vs. real, or different models per role).
- **`complete_json`** — a free function (not a trait method, to preserve dyn-compatibility) that runs
  a request in JSON mode and deserializes the reply into a typed `T`. This is the dispatcher's path
  to a typed `DispatchDecision`.
- **Types** (`types.rs`) — `CompletionRequest`/`CompletionResponse`, `Message`/`Role`, `ToolDef`,
  `ToolInvocation`, `ResponseFormat` (`Text`|`Json{schema}`), `FinishReason`, `Usage`. A
  chat-completions-shaped, provider-agnostic narrow waist.
- **`MockProvider`** (`mock.rs`) — scriptable test double: hand it a queue of `CompletionResponse`s,
  it pops one per call and records every request, so a scenario can assert on both what the system
  did with a response and what it sent.

## Dependencies

- Depends on: `liberado-common` is **not** required here — this crate is self-contained
  (`serde`, `async-trait`). Concrete backends live in their own crates.
- Depended on by: `provider-deepseek` (implements `Provider`), `dispatcher`, `executor`,
  `orchestrator`, `mcp` (tool types), `cli`.

## Design notes

- The two capabilities the model-capability floor cares about — **tool-calling** and **structured
  output** — are the only things the vocabulary must express.
- A `ProviderError::Decode` from `complete_json` is the hook the dispatcher maps into its
  retry/repair/escalate flow.

## Tests

`tests/coverage.rs` + `mock.rs` inline tests (scripting, request recording, `complete_json` decode).
