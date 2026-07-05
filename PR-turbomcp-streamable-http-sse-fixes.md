# fix(http): Streamable HTTP client hangs against spec-compliant SSE servers

**Branch:** `fix/streamable-http-post-response-stream-hang` (pushed to origin)
**Base:** `main`
**Commits:** 2 (`919f144`, `d47a8ca`)

## Summary

The Streamable HTTP client transport can hang for the full request timeout (60s by
default) against MCP servers that behave in ways the spec explicitly permits. Two
independent, previously-undetected bugs in the same code path caused this — either one
alone is enough to reproduce the symptom, and together they made the failure look like a
single, confusing "the server never responds" hang with no useful error.

Found and fixed while integrating against a real, publicly-reachable MCP server over
Streamable HTTP. Both bugs are general — they'll affect any server exhibiting the same
(spec-legal) behavior, not just the one that surfaced them.

## Bug 1: `send()` blocks until the POST-response SSE stream closes, not until it has the response

When a server responds to a POST with `Content-Type: text/event-stream` instead of a
direct JSON body, `StreamableHttpClientTransport::send()` reads that stream in a loop
that only exits when the stream closes, a read error occurs, or a buffer-size cap is
hit — even after it has already parsed and queued the JSON-RPC response the caller is
waiting for.

Per the MCP spec ([Streamable HTTP transport](https://modelcontextprotocol.io/specification/2025-06-18/basic/transports#sending-messages-to-the-server)):

> After the JSON-RPC *response* has been sent, the server **SHOULD** close the SSE
> stream.

This is a **SHOULD**, not a **MUST**. A fully spec-compliant server is not required to
close the stream immediately — it may keep it open (e.g., to send further related
messages, or simply due to implementation timing). Any such server hangs every request
through this client until the *unrelated* operation-level timeout eventually fires,
which is not a real fix, just a slow one.

**Fix:** `process_post_sse_event` now reports whether the event it just queued is the
JSON-RPC *response* correlated to the outgoing request's `id` — as opposed to some other
message (a request or notification) the server chose to send first over the same stream,
which the spec also explicitly permits:

> The server **MAY** send JSON-RPC *requests* and *notifications* before sending the
> JSON-RPC *response*.

`send()` uses that signal to break out of the read loop as soon as the correlated
response is found, rather than waiting for the stream to end. A notification arriving
first no longer terminates the loop early; a response with a mismatched `id` (e.g. a
stray late arrival for a different in-flight request) is also correctly not treated as
the correlated response.

## Bug 2: SSE event-boundary detection assumes bare `\n\n`, silently drops everything from a CRLF-emitting server

Both SSE read loops (the standalone GET stream and the POST-response stream) locate
event boundaries with `buffer.find("\n\n")`. Per the [WHATWG SSE
specification](https://html.spec.whatwg.org/multipage/server-sent-events.html#event-stream-interpretation),
which the MCP spec's resumability section cites directly:

> Lines must be separated by either a U+000D CARRIAGE RETURN U+000A LINE FEED (CRLF)
> character pair, a single U+000A LINE FEED (LF) character, or a single U+000D CARRIAGE
> RETURN (CR) character.

A server that terminates lines with `\r\n` (confirmed via a live capture — a real server
returns `event: message\r\ndata: {...}\r\n\r\n`) produces a `\r\n\r\n` boundary between
events. That string contains no `\n\n` substring at all (`\r`, `\n`, `\r`, `\n` — never
two consecutive LFs), so the boundary search never matches. The event is silently
dropped: no parse error, no warning, nothing queued. From the caller's side this is
indistinguishable from bug 1's symptom — both present as "the call just never
completes" — which is why fixing only one of them wasn't sufficient to resolve the
reproduction.

**Fix:** normalize `\r\n` and lone `\r` to `\n` per chunk before appending to the read
buffer, in both loops, via one small shared helper. A chunk boundary that happens to
land exactly inside a `\r\n` pair produces one extra blank line in the reassembled
buffer in the rare worst case — harmless, since the per-field event parser already
treats blank lines as a no-op, so this stays a simple per-chunk normalization rather than
carrying a pending-CR byte across chunk boundaries.

## Testing

- `cargo fmt`, `cargo check -p turbomcp-http`, `cargo clippy -p turbomcp-http --all-targets -- -D warnings`, and `cargo test -p turbomcp-http --lib` all clean (13 tests, up from 9 — 4 new, 2 updated for the changed return type).
- New tests: a notification arriving before the real response no longer ends the read
  loop early; a response with a mismatched `id` is correctly not treated as the
  correlated response; the line-ending normalization helper (CRLF, lone CR, and
  unmodified-LF cases); a full CRLF-terminated event parsed end-to-end through
  `process_post_sse_event`.
- **Live-verified against a real Streamable HTTP MCP server** before and after each fix,
  isolating which bug produced which part of the symptom:
  - Before either fix: `initialize()` times out at 60s.
  - After fix 1 only: `send()` returns quickly (confirmed via debug tracing — no more
    blocking on stream closure), but the handshake *still* times out, because zero
    events were ever recognized in the stream (bug 2 masked by bug 1 up to this point).
  - After both fixes: connects and completes `initialize()` in under 300ms end-to-end,
    including the follow-up `notifications/initialized` round-trip.
- Re-checked both fixes against the exact spec language quoted above after
  implementation, not just before — to confirm the fix's behavior (stop reading once the
  correlated response is found; accept all three SSE line-ending forms) matches what the
  spec actually requires of a conformant client, rather than just what made the one
  reproduction case pass.

## Notes for review

- Both fixes are scoped to `crates/turbomcp-http/src/transport.rs` only — no public API
  changes for consumers of `turbomcp-client`. `process_post_sse_event`'s return type
  changed from `TransportResult<()>` to `TransportResult<bool>`, but it's a private
  (non-`pub`) associated function, so this is not a breaking change for downstream
  crates.
- The GET-based standalone SSE loop gets the same line-ending normalization fix as the
  POST-response loop for consistency and because it has the identical `\n\n`-search
  pattern, even though the reproduction case that surfaced this specifically exercised
  the POST-response path. Untested against a live server that both keeps a GET stream
  open long-term *and* emits CRLF, but the fix is a straightforward application of the
  same, spec-grounded normalization.
