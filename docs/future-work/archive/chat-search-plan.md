---
kind: plan
status: implemented
authority: advisory
domain: chat
canonical_for: chat-search
open_items: false
---

> **Archived.** This plan is not current truth. Open work lives in [backlog.md](../backlog.md) and [roadmap.md](../../roadmap.md). See [doc-authority.md](../../spec/reference/doc-authority.md).

# Chat Search — Design & Roadmap

**Status**: Tier 1 done (2026-07-05) — see "Tier 1" below for what actually shipped, which
refined a few details from this doc's original sketch (grouped-by-conversation results with a
match per message, not one flat row; the `regex` crate directly rather than ripgrep's own
`grep-searcher`/`grep-regex` library crates; a new `liberado-chat-search-mcp` turbomcp server so
the **dispatcher** can search history mid-reasoning, not just the human via the webui). Tiers 2-3
remain captured, not scheduled.

## Motivation

Conversation history has no content search today — only a chronological list by title in the
sidebar. As history accumulates, finding a past exchange means scrolling and guessing at titles.
The ask: a real search box at the top of the sidebar's conversation list, searching message
*content*, with three tiers of increasing sophistication — lexical (ripgrep-powered), BM25-ranked,
and vector/semantic — shipped in that order, stopping whenever the simpler tier proves sufficient.

## Current state (grounding, checked before writing this)

- **Storage**: `liberado-conversation-store`'s `JsonlStore` — one file per conversation at
  `<root>/<conversation_id>.jsonl`; line 0 is the conversation header, every following line is one
  self-describing JSON `Record` (`Header`/`Node`). The module's own doc comment says the log "stays
  greppable" — this was already anticipated, not a repurposing.
- **`ConversationStore` trait** (`crates/conversation-store/src/store.rs`) exposes `create`,
  `append`, `path` (one conversation's root→leaf message chain), `list` (headers only — id/title/
  timestamps, **no content**), `set_title`. No search/query capability exists at any level today.
- **API/UI**: `GET /api/conversations` (consumed by `sidebar.rs`'s `fetch_conversations`) returns
  headers only; `ConvItem` renders title + relative timestamp. Nothing in the current webui touches
  message content outside an open conversation.
- **Dependency check**: no `tantivy`, no ripgrep library crates (`grep-searcher`/`grep-regex`/
  `grep-matcher`), no vector/embedding library is a *direct* dependency of any `liberado-*` crate.
  (`tantivy` does appear in `Cargo.lock`, but only as an already-present *transitive* dependency
  somewhere in `liberado-webui`'s graph, unrelated to search.) This is a clean-slate feature — no
  existing groundwork to build on or accidentally duplicate.

## Design: three tiers, shipped in order

The wire contract is designed to survive all three tiers unchanged — the frontend is built once
against a `ConversationSearchResult` shape, and gets *better answers* as later tiers land server-side
without ever needing its own changes. That stability is the reason to nail the API shape early even
though only Tier 1 ships first.

```rust
// crates/chat-client-contract/src/wire.rs — as actually shipped
pub struct SearchMessageMatch {
    pub node_id: String,
    pub author: String,
    pub content_snippet: String,
    pub created_at: String,
}
pub struct ConversationSearchResult {
    pub conversation_id: String,
    pub title: Option<String>,
    pub created_at: String,
    pub matches: Vec<SearchMessageMatch>,  // every matching message in this conversation, not just one
}
pub struct ConversationSearchResponse {
    pub results: Vec<ConversationSearchResult>,
    pub total_found: usize,  // count BEFORE truncation to `limit` — an honest "N of M", not `limit` echoed back
}
```
Grouped by conversation rather than one flat row per match: a query hitting several messages in
the same conversation shows all of them with their own snippets, instead of collapsing to one row
or duplicating the conversation N times. `message_id` (via `node_id`) and no `score` field yet —
Tier 2 would add ranking; the shape doesn't need to change to carry it (`score: Option<f32>` slots
in without breaking the rest).

New endpoint (stable across tiers): `GET /api/conversations/search?q=<query>&regex=<bool>&limit=<n>`.

### Tier 1 — Lexical (done, 2026-07-05)

The store's on-disk layout was already built to be greppable — this tier uses that, via a new
shared crate, `liberado-chat-search`, consumed by two front-ends (not duplicated):

- `GET /api/conversations/search` (`crates/server/src/api.rs`) — for the webui's sidebar search box.
- `liberado-chat-search-mcp` (new `crates/chat-search-mcp/`, a `turbomcp`-based stdio MCP server) —
  so the **dispatcher** can search chat history mid-reasoning, registered in `topology.toml` as a
  workspace-local `stdio` MCP (not "managed" — managed MCPs are cargo-installed from an external
  git repo only, per `crates/mcp-forge`; a workspace crate just points `command` at its own binary).

**Library choice, resolved**: Rust's `regex` crate directly (the same engine ripgrep itself is
built on), applied per-message to the already-JSON-parsed `content` field — not ripgrep's own
`grep-searcher`/`grep-regex` library crates (built for streaming arbitrary files line-by-line,
overkill for data that's already structured JSONL we parse ourselves) and not shelling out to the
`rg` binary (keeps the daemon single-binary, no external tool dependency). Still the same
underlying matching power/semantics the "ripgrep-powered" ask was after.

**Query semantics** (a design call made explicitly, not left open): `regex: bool` param, default
`false`. Literal mode splits the query on whitespace into terms (`"quoted phrases"` count as one
term) and **AND**s them — a message matches only if it contains every term (case-insensitive
substring). This is what "vague recall of a topic" wants: narrowing by a few half-remembered
keywords, not flooding results with an OR. Regex mode treats the whole query as one case-insensitive
Rust regex pattern. OR/boolean-expression support was **not** built — noted in
`crates/chat-search/src/query.rs`'s own doc comment as a natural, additive future extension.

- **Scope**: scans every `<root>/*.jsonl` file fully (skipping empty-content/tool-call-only
  messages and any line that fails to parse — a best-effort search path, not the authoritative
  store), sorts newest-conversation-first, then truncates to `limit`.
- **Ranking**: none — recency order only. Honest limitation of this tier, not a bug.
- **Cost**: no persistent index to build or maintain; always reflects on-disk truth exactly; cheap
  at personal/homelab scale (this project's own stated scale assumption).

### Tier 2 — BM25 ranking (only if Tier 1 proves insufficient)

- **Motivation**: once history is large, substring matching returns too many/poorly-ordered hits.
  Needs actual relevance ranking (term frequency, inverse document frequency, length
  normalization).
- **Library**: `tantivy` — an embedded, single-process, Lucene-like engine with native BM25
  scoring. Fits the "one binary, no separate service" deployment story this project already keeps
  to. (Incidentally already transitively present in the dependency graph, so this wouldn't be a
  wholly new dependency family, even though it isn't used for anything today.)
- **Granularity**: index at the *message* level (not per-conversation), so a result can point at
  the specific message that matched (`message_id` becomes populated) — not just "somewhere in this
  conversation."
- **The real cost, stated plainly**: an index needs building and, more importantly,
  **incrementally maintaining** as `append()` writes new messages — either inline in the write
  path (adds latency to every append, simplest to reason about) or async (a background reindex
  queue watching the JSONL directory, similar in spirit to `VaultEventSource`'s watch pattern this
  codebase already has, but eventually consistent and another moving part that can drift/need a
  rebuild). This is a genuinely bigger lift than Tier 1 and should not be started casually — the
  decision between inline vs. async indexing needs its own explicit call when this tier is
  actually scheduled, not decided in this doc.

### Tier 3 — Vector/semantic search (deferred, not scheduled)

- **Motivation**: BM25 and lexical search both miss semantically-related-but-differently-worded
  content (e.g. "that thing about the tailscale firewall" should find a message about "Windows
  Defender blocking liberado.exe" despite zero shared keywords).
- **Needs, both genuinely open**:
  - An embedding source — either the existing `Provider` abstraction if/when it exposes an
    embeddings endpoint, or a dedicated local embedding model (small ONNX/`candle`-based, to avoid
    a per-query API round-trip's cost/latency/privacy tradeoff for what's fundamentally a personal
    homelab tool). Not decided here.
  - A vector store — at personal/homelab scale (thousands, not millions, of messages), a naive
    flat-file store with brute-force cosine similarity is very likely sufficient. **Do not reach
    for a real vector database or an ANN library (`hnsw`, `usearch`, etc.) preemptively** — that's
    solving a scale problem that doesn't exist yet, the same "don't build what you don't need"
    call this project has made elsewhere (see the deferred pub/sub idea in
    [`a2a-protocol-idea.md`](../ideas/a2a-protocol-idea.md)).
- **Recommendation**: do not build this until Tier 1 (and maybe Tier 2) are live *and* a concrete,
  real "I searched for X and lexical/BM25 genuinely couldn't find it" case actually shows up.
  Captured here as a deferred idea, not a commitment.

## Frontend design (done)

- A search input at the top of the sidebar's conversation list (`sidebar.rs`, above the
  conversation list, below "+ New Chat"/collapse). No debounce in v1 — `use_resource` re-fires per
  keystroke against a local file scan, fast enough in practice; noted as a follow-up if it's ever
  felt to be too chatty.
- Non-empty query swaps the list from "all conversations by recency" to search results (a new
  `SearchResultItem` component), each showing every matching message's snippet, not just one —
  clicking still opens the conversation at its default leaf (jump-to-specific-message via
  `node_id` is not built — the `Chat` component has no scroll-to/highlight primitive yet; a natural
  follow-up, not built speculatively). Clearing the box reverts to the normal list.
- Same three-state loading pattern already used by the conversation list and MCP panel
  (`Some(Ok(_))` / `Some(Err(_))` / `None`) — no new pattern invented.

## Suggested sequencing

1. ~~**Tier 1 first**, as its own slice~~ — done.
2. **Tier 2 only if Tier 1's lack of ranking genuinely becomes a problem in practice** — not
   pre-emptively.
3. **Tier 3 deferred indefinitely** until a concrete need is felt, per its own section above.

## Files (Tier 1, as built)

- `crates/chat-search/` (new, `liberado-chat-search`) — `query.rs` (parsing/matching), `scan.rs`
  (directory scan, snippet extraction, `SearchResults { matches, total_found }`), both with unit
  tests. No daemon dependencies — consumed by both front-ends below.
- `crates/chat-client-contract/src/wire.rs` — `SearchMessageMatch`, `ConversationSearchResult`,
  `ConversationSearchResponse`.
- `crates/server/` — `AppState.conversations_root`, `GET /api/conversations/search` (`api.rs`),
  route registration (`lib.rs`).
- `crates/chat-search-mcp/` (new, `liberado-chat-search-mcp` binary) — the turbomcp stdio server
  exposing `search_conversations` to the dispatcher.
- `crates/webui/src/components/sidebar.rs`, `crates/webui/src/styles/main.css` — search input,
  `SearchResultItem`, CSS.
- `config.example/topology.toml`, `config.example/policy.toml` — commented `[[mcps]]`/`[[grants]]`
  examples (grants `chat-search` to the `dispatcher` component only, not `main-agent` — same tier
  as `code-dispatch`; adding it to `main-agent` too is a one-line follow-up if direct chat should
  also search history).
- Root `Cargo.toml` — `regex` and `liberado-chat-search` in `[workspace.dependencies]`.
