# Issue draft — Expose request `_meta` end-to-end on `tools/call`

> Draft for an issue against **Epistates/turbomcp**. Tidy and open on GitHub.
> Reference branches (on my fork):
> - turbomcp: https://github.com/ForrestThump/turbomcp/tree/feature/tool-call-request-meta (`b9a0635`)
> - turbovault (downstream consumer): https://github.com/ForrestThump/turbovault/tree/feat/write-note-request-meta (`1b0f03e`)

---

## Title

Surface request `_meta` to tool handlers end-to-end (client can send it; router drops it)

## Summary

The MCP spec defines `_meta` on requests as an open, implementation-defined metadata object — the natural place for **out-of-band** per-call data that is *not* part of a tool's `arguments` and is *not* shown to the model (provenance, correlation/trace IDs, tenancy, idempotency keys). Today `_meta` is unreachable end-to-end in turbomcp:

- **Client side:** `Client::call_tool` / `call_tool_response` hard-code `_meta: None` when building the `CallToolRequest`, so a caller has no way to attach it.
  - `crates/turbomcp-client/src/client/operations/tools.rs`
- **Server side:** the `tools/call` router reads only `params["arguments"]` and passes the borrowed `RequestContext` straight through — the request's `_meta` is dropped before the handler runs, so a tool handler can never see it.
  - `crates/turbomcp-core/src/router.rs` (the `"tools/call"` arm)

`RequestContext` already carries a `metadata: Map<String, Value>` with `get_metadata`/`with_metadata`, so the surface area to fix this is small — there just isn't a path from the wire `_meta` into it.

## Motivating use case

A reactive system that watches a vault/store and also writes to it via MCP needs to **attribute** each change to its originator, so it doesn't react to its own writes (loop-breaking). The clean way is for the agent's writes to carry provenance (a source id + correlation id) that lands on the server's **audit log**, which a separate process reads back. That provenance is exactly out-of-band metadata: it must not pollute the tool's argument schema and must not be visible to the model — i.e. it belongs in `_meta`.

A concrete consumer is implemented on the turbovault branch above: the `write_note` MCP tool reads the request `_meta` the router surfaces and records it on the write's audit-log entry (turbovault itself stays agnostic — it just persists request `_meta`). That branch won't compile against published turbomcp until this change lands, which is part of why I'm raising it here rather than just vendoring a workaround.

## Proposed change (implemented on the reference branch)

Fully additive; no existing call site changes behavior.

1. **turbomcp-client** — add `call_tool_with_meta` and `call_tool_response_with_meta`; the existing `call_tool` / `call_tool_response` delegate through them with `None`, so request `_meta` has a single set-point.
2. **turbomcp-core router** — the `tools/call` arm lifts the request's `_meta` into `RequestContext.metadata` under a new exported const `REQUEST_META_KEY` (`"_meta"`), so handlers read per-call metadata with **no handler-signature change**. When `_meta` is absent, the key is simply not present.
3. **turbomcp (umbrella) + prelude** — re-export `REQUEST_META_KEY` so handlers can reference the canonical key.

### Tests added

- `turbomcp-core`: router surfaces `_meta` into the handler ctx; and is absent when not sent.
- `turbomcp-client`: full client→server round-trip over the in-process `channel` transport — `call_tool_with_meta` serializes `_meta`, the router surfaces it, a handler echoes it back, the client receives it.

## Backward compatibility

- Additive only. Existing `call_tool*` signatures and behavior are unchanged.
- Servers that ignore `_meta` are unaffected; handlers opt in by reading `ctx.get_metadata(REQUEST_META_KEY)`.

## Open questions for maintainers

1. **Key naming / namespacing** — `REQUEST_META_KEY = "_meta"`. Acceptable, or would you prefer a namespaced/reserved key to avoid collision with user metadata?
2. **Scope** — this branch surfaces `_meta` for `tools/call` only. Should `resources/read` and `prompts/get` get the same treatment for symmetry?
3. **Typed vs free-form** — `_meta` is passed through as `serde_json::Value`. Do you want a typed accessor/helper on `RequestContext`, or is the raw `Value` under a known key sufficient?
4. **Client ergonomics** — separate `*_with_meta` methods vs. a builder on the existing call path. I chose additive methods to avoid touching existing signatures; happy to reshape.

## Offer

I have this implemented and tested on the branch above (and a downstream consumer in turbovault). **If you'd like it as a PR (or two — client + core), say the word and I'll open them against `main`.**
