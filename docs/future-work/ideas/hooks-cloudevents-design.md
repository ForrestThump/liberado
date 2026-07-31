# Hooks event envelope — CloudEvents-compliant design

**Status**: Design, not yet implemented — captured per explicit direction ("spec out and design the
hook system before we write code for it"). `crates/common/src/event.rs`'s current `Event`/
`EventPayload` types are unchanged; this doc is the target shape for when hooks/cron
(`docs/roadmap.md`'s Phase 3, `docs/roadmap/mcp-forge-backlog.md`'s inbound-trigger entries)
are actually picked up.

## Why CloudEvents, and why this isn't solved by MCP or A2A

Grew out of a live design conversation (2026-07-04) prompted by comparing Liberado's own internal
tooling (a wake-up scheduler, task tracking, a proposal mechanism) against MCP and A2A. Two
independent conversations with other LLM instances — saved verbatim at
`docs/ideas/archive/mcp_acp_protocol_difference_conversation.md` and `docs/ideas/archive/grok_take_on_hooks.md` —
converged on the same conclusion: **no standard protocol exists for "an external, non-agent event
(cron tick, webhook) wakes/activates an LLM/agent, with a defined contract for new-session-vs-resume
and default capabilities."** MCP is capability-exposure (client calls, server responds) — a cron
tick isn't a client calling anything. A2A is agent-to-agent task delegation — a cron tick isn't an
agent, has no AgentCard, no reasoning to offer. Neither has a slot for "an event with no agent
behind it needs to activate one."

**CloudEvents (CNCF)** is the closest real, adopted standard — not agent-specific at all, just a
vendor-neutral envelope for "something happened" (`id`, `source`, `type`, `time`, `data`). It
doesn't solve the "wake the model" half either, but it solves the layer underneath: a standard
shape for the trigger itself. `liberado_common::Event`/`EventPayload` (`life-os-architecture.md`
§5, Decision 6) is already a bespoke version of exactly this idea — this doc is the plan to make it
actually CloudEvents-compliant (core attributes + the spec's own extension-attribute mechanism)
rather than reinvent the envelope, for the same reason `a2a-protocol-idea.md` favors building
against A2A's real spec over rolling a new one: cheap compliance now, real interop later, no reason
to reinvent a solved layer.

## The organizing principle: does the model need this to reason about the task?

Not "CloudEvents core vs. extension" and not "envelope vs. payload" — the field that actually
decides where something goes is: **would a model reasoning about what to do in response to this
event need to read this field, or is it plumbing a deterministic layer (dedup, loop-breaking,
routing) consumes before the event ever reaches an LLM?**

- Plumbing (dedup/routing, consumed by the daemon, never by the model's own reasoning) → CloudEvents
  core attributes, or a Liberado routing extension if there's no core-attribute fit.
- Substance (what the model would actually read to decide how to react) → inside `data`.

This deliberately keeps "the model's job" narrow: envelope = boilerplate a model has plausibly seen
before in training data (CloudEvents JSON is common) and can gloss over; `data` = the actual content
of *this* occurrence, one clearly-scoped place to look, not scattered across a dozen top-level
fields of mixed relevance.

## The shape

```rust
pub struct Event {
    // --- CloudEvents core — plumbing/routing, not model-reasoning material ---
    pub id: String,                  // fresh ULID per occurrence — CloudEvents' own dedup key,
                                      // NOT the same axis as correlationid (see below)
    pub source: String,              // e.g. "turbovault-subscription" — already a valid URI-
                                      // reference as a bare token, no change needed
    pub specversion: String,         // "1.0"
    #[serde(rename = "type")]
    pub event_type: String,          // reverse-DNS: "dev.liberado.vault.decision.logged"
    pub time: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,     // vault path, when there is one (was EventPayload.path)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub datacontenttype: Option<String>, // "application/json" when data is set

    // --- Liberado routing extension (CloudEvents extension-attribute mechanism: top-level,
    // lowercase-alphanumeric name, no underscore) — still plumbing, not model-reasoning material ---
    pub correlationid: String,       // Decision 6 loop-breaking / idempotent-handler key,
                                      // required on every event, reused across a whole causal
                                      // chain (write -> event -> reaction -> follow-up write) —
                                      // NOT unique-per-occurrence like `id` is

    // --- Everything the model actually needs to reason about the occurrence ---
    #[serde(skip_serializing_if = "serde_json::Value::is_null")]
    pub data: serde_json::Value,     // conventionally shaped, not rigidly typed (see below):
                                      // { provenance: {...}, hints: { summary: "..." }, ...
                                      //   domain-specific fields }
}
```

### `id` vs. `correlationid` — two different axes, do not conflate

CloudEvents' `id` (+ `source`) must be unique per single occurrence — its job is exactly-once
delivery/dedup ("have I seen *this* event before"). Liberado's `correlation_id` is a causal-chain
key shared across *multiple distinct events* over the lifetime of one reaction chain — Decision 6's
loop-breaking mechanism, a different concern entirely. Both stay top-level (both are plumbing the
daemon's own attribution logic consumes, neither is something a model needs to reason with), but
they must never be minted from the same value or reused for each other's purpose.

### Reverse-DNS `type` namespace: `dev.liberado.*`

Decided 2026-07-04: `liberado.dev` isn't a domain the project owns today and may never need to be,
but it isn't taken and costs nothing to anchor the namespace to now (trivial to change later if
that ever stops being true — this is a naming convention, not a functional dependency on the domain
actually resolving to anything). Reverse-DNS reverses the domain's segments, so `liberado.dev`
becomes `dev.liberado`, not `liberado.dev`.

Concrete translations of names already in `crates/common/src/event.rs`:

| Current `event_type` | `dev.liberado.*` type |
|---|---|
| `"DecisionLogged"` | `dev.liberado.vault.decision.logged` |
| `"InboxNoteSettled"` | `dev.liberado.vault.inbox.settled` |
| `"NightlySweep"` (systemd timer) | `dev.liberado.cron.nightlysweep` |

`event_source`'s constants (`TURBOVAULT_SUBSCRIPTION`, `SYSTEMD_TIMER`, `GIT_HOOK`, `DOCKER_EVENT`)
are a different axis (`source`, not `type`) and are unaffected by this — already valid CloudEvents
`source` values as bare tokens, no change needed.

### `data` stays loosely typed — a documented convention, not an enforced schema

This preserves something already load-bearing in the current design, per `event.rs`'s own module
doc: "any hook-capable system must be able to mint a valid event without linking our crates" — a
bash cron script or a raw `curl` from a systemd unit needs to be able to POST a valid event without
knowing Rust type shapes. If `data` became a strict `EventData` struct, an external hook author
would need to know `WriteProvenance`'s exact schema to emit anything at all. Keeping `data` as
`serde_json::Value`, with `provenance`/`hints` as a *documented convention* the daemon populates
when it has them (not a schema external producers must satisfy), means a plain webhook script can
still send `{"event_type": "...", "data": {"whatever": "it wants"}}` and produce a valid event,
while the daemon-originated (vault-watch) path can populate the richer conventional shape.

## Open / not yet decided

- Exact shape of `hints` beyond `summary` (e.g., a suggested urgency/consequence signal for the
  dispatcher) — not designed yet, no evidence of need beyond the one field that exists today.
- Whether `source` values should eventually become fuller URIs (e.g.
  `dev.liberado.daemon/turbovault`) rather than bare tokens — optional polish, not required for
  compliance (bare tokens already parse as valid relative references); not decided either way.
- Migration path from the current `Event`/`EventPayload` shape to this one — no consumers exist yet
  outside the vault-watch path and the (not-yet-built) hooks/cron event sources, so this is likely a
  clean swap rather than a versioned migration, but not confirmed.
- Whether `dataschema` (CloudEvents' optional pointer to a schema `data` adheres to) is worth
  populating given `data`'s deliberately-loose typing above — leaning no, not decided.

## Companion to

- `crates/common/src/event.rs` — the current `Event`/`EventPayload` this design replaces.
- `docs/ideas/archive/mcp_acp_protocol_difference_conversation.md` — the MCP/ACP/A2A protocol comparison
  that started this thread.
- `docs/ideas/archive/grok_take_on_hooks.md` — independent corroboration (different LLM) of the same
  conclusion: no wake-protocol standard exists, CloudEvents is the right foundation layer.
- `docs/roadmap/mcp-forge-backlog.md` — where hooks/cron's inbound-vs-outbound, core-vs-mcp-forge
  split is sorted; this doc is the envelope shape for whichever pieces of that get built.
- `docs/roadmap.md` — Phase 3 (cron as a bus listener, vault-decoupling behind an
  event-source/hook trait) — the roadmap slot this design is for.
- `docs/architecture/overview.md` — "MCPs vs hooks," the existing decided distinction this design
  extends.
