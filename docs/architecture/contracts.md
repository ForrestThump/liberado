# Contracts — the narrow waists

**This page is the architecture.** Liberado's stability does not come from any component being
finished; it comes from a small set of frozen contracts that everything composes through. Every
crate on either side of one of these seams is replaceable without rewrite regret — that is the
whole loose-coupling bet, stated as an inventory instead of a slogan.

Rules of thumb:

1. **A dependency that is not one of these waists must justify itself.** "Should X depend on Y?"
   → if Y is not on this list and X is not Y's composition root, the answer starts at no.
   (Mechanically enforced where possible: `crates/test-support/tests/layer_rules.rs`.)
2. **Changing a contract is an event, not an edit.** Each has a blast-radius note below. Additive
   change (new optional field, new trait with default impl) is cheap; signature change means
   sweeping every implementor and consumer in one pass — budget for it.
3. **Per-contract depth lives with the defining crate** (`crates/<name>/ARCHITECTURE.md`), not
   here. This page stays a one-screen-per-contract index; if a section outgrows that, move the
   detail down into the crate doc and leave the summary.

Layer vocabulary used below (and in every crate's `[package.metadata.liberado] role`):
**foundation → kernel → stores / domain packs → services / surfaces → composition roots.**

---

## The inventory

| Contract | Kind | Defined in | The seam it freezes |
|---|---|---|---|
| [`Provider`](#provider) | trait | `liberado-provider` | inference: who does the thinking |
| [`ToolRuntime`](#toolruntime--runtimefactory) | trait | `liberado-executor` | acting: what tools exist and how they run |
| [`EventSource`](#eventsource) | trait | `liberado-common` | perceiving: what wakes the daemon |
| [`DomainPackRunner`](#domainpackrunner) | trait | `liberado-session` | goal sessions: how a domain plugs into the kernel |
| [`ConversationStore`](#conversationstore) | trait | `liberado-conversation-store` | the chat lens onto a session (see [`sessions.md`](sessions.md)) |
| [`SessionRecordStore`](#conversationstore) | trait | `liberado-session` | the kernel lens onto a session |
| [`ConfigSource`](#configsource--opaque-pack-sections) | trait | `liberado-config-loader` | layered config loading |
| [`Notifier`](#notifier) | trait | `liberado-notify` | human-facing notification channels |
| [HTTP/SSE wire contract](#the-httpsse-wire-contract) | DTOs + endpoints | `chat-client-contract` + `docs/reference/api.md` | every surface ↔ the daemon |
| [MCP + `WriteProvenance`](#mcp--writeprovenance) | protocol + `_meta` | `liberado-mcp` / Turbomcp | agent ↔ external tools; loop-breaking |
| [`CapabilitySet` narrowing](#capabilityset-narrowing) | semantics | `liberado-common` | authority only ever shrinks |

Pack-level contracts (same discipline, scoped to one domain): `CoderBackend` and the coder DTOs in
`liberado-coder-core` — the coding pack's own narrow waist toward PR factory and evals.

---

## Provider

- **Defined**: `liberado-provider` (`Provider` trait + `MockProvider`). No HTTP in the trait crate.
- **Implemented by**: `provider-openai-compat` (one config-driven OpenAI-compatible backend,
  hot-swappable model id); `MockProvider` for every test (Decision 16).
- **Consumed by**: executor, dispatcher, main-agent, coder roles, tuner — anything that thinks.
- **Promise**: provider-agnostic inference (Decision 13). Nothing outside a composition root may
  name a concrete provider type in a real (non-dev) dependency.
- **Blast radius if changed**: every agent loop and every mock-driven test. Additive only,
  effectively.

## ToolRuntime / RuntimeFactory

- **Defined**: `liberado-executor`. `ToolRuntime` = catalog + invoke; `RuntimeFactory` builds one
  scoped to a capability set.
- **Implemented by**: `liberado-mcp` (`TurbomcpRuntime` — real MCP tools, provenance in `_meta`),
  `coder-tools` (coding limb), `scratchpad`, `RiskGatedToolRuntime` (the guard decorator),
  test doubles in `test-support`.
- **Promise**: this is *the* domain limb. Coding tools and MCP tools are interchangeable from the
  executor's point of view; a new domain is "different runtime + different verifiers", never a
  second agent engine.
- **Blast radius**: the executor loop, every runtime impl, the risk-gating decorator.

## EventSource

- **Defined**: `liberado-common` (`event.rs`); the daemon fans all sources into one channel.
- **Implemented by**: `VaultEventSource` (vault watch), `liberado-cron`; the webhook receiver
  (`POST /api/hooks/{name}`) injects into the same channel push-style via `Daemon::event_sender()`.
- **Promise**: the daemon consumes events without knowing where they came from — this is what
  demotes TurboVault from hard dependency to default-privileged plugin (Decision 19).
- **Blast radius**: daemon loop + all sources; historically cheap (vault-watch was refactored onto
  it with the prior test suite passing unchanged).

## DomainPackRunner

- **Defined**: `liberado-session` (with `GoalSpec`, `SessionEvent`, store/hub; served as
  `/api/goals*`).
- **Implemented by**: `CodingSessionPack` (`coder-agent`), `LifeOpsDemoRunner` (the second-domain
  proof — no `coder-*` anywhere in its path).
- **Promise**: the kernel never imports git/cargo/sandbox types; a pack that only makes sense for
  coding belongs under `coder-*`, not here (design rule #10, the pigeonhole detector).
- **Blast radius**: every registered pack + the goals API. The `SessionEventKind` vocabulary is
  now also the chat stream's wire language (converged 2026-07-11), so variant changes reach every
  surface — additive only.

## ConversationStore + SessionRecordStore

**Two traits, one implementation** — `liberado-session-store::SessionStore` implements both. This is
not an accident of history; it is a layer rule doing its job. See [`sessions.md`](sessions.md).

- **Defined**: `ConversationStore` in `liberado-conversation-store` (the trait + the message-node DAG
  types — Decision 17). `SessionRecordStore` in `liberado-session` (records + events, kernel types
  only).
- **Why two**: the session kernel may not depend on `liberado-provider`, so it **cannot know what a
  `Message` is**. A store that must hold both provider messages and pack events therefore lives
  *above* the kernel (`liberado-session-store`, role `store`) and reaches *down* to the kernel's
  trait. One engine, one id space, two typed views.
- **Implemented by**: `liberado-session-store::SessionStore` (production).
  `liberado-conversation-store::JsonlStore` is the pre-convergence implementation, now **test-only** —
  which is itself a hazard worth knowing about: `main-agent`'s chat tests run against `JsonlStore`
  while production runs on `SessionStore`.
- **Consumed by**: `main-agent` (`ChatSessions` rehydrates per turn, persists only on success),
  `GoalSessionHub`, the server API, `chat-search`.
- **Promise**: cancelled turn = clean on-disk no-op; **monotonic ids**, so file order == id order and
  `leaf_path` never walks from the wrong leaf; the message body type is reused from
  `liberado-provider` so content has a single definition; a session's log is **self-contained**
  (which is what makes fork-by-copy the right call).
- **Blast radius**: all session persistence + search + forking + any exporter. The JSONL format is
  effectively a second, on-disk contract.

## ConfigSource + opaque pack sections

- **Defined**: `liberado-config-loader` (`ConfigSource` + `ChainLoader`; `config` assembles and
  validates the resolved `Config`, Decision 14 fail-fast).
- **Promise (updated 2026-07-11)**: the config stack is **pack-agnostic**. A domain pack's section
  (`[tuning.coder]`) rides through as an opaque `toml::Value`; the pack parses + validates it at
  composition time (`liberado_coder_core::CoderTuning::from_value`). The daemon never writes
  config.
- **Blast radius**: everything boots through this; but *pack* config changes are now contained to
  the pack.

## Notifier

- **Defined**: `liberado-notify` (trait + proposal notification with Approve/Revise/Reject).
- **Implemented by**: `TelegramNotifier`; `telegram-approvals` answers the buttons (approve/reject
  pure code; revise redrafts content only, never grants).
- **Promise**: trait-only coupling from the engine side (`executor`'s risk gate holds a
  `dyn Notifier`); concrete channels appear only in composition roots and `#[ignore]`d live tests.
- **Blast radius**: small; add channels freely.

## The HTTP/SSE wire contract

- **Defined**: `chat-client-contract` (wire DTOs + `SseDecoder`) and `docs/reference/api.md`
  (endpoints: chat, stream, conversations, models, goals, hooks).
- **Consumed by**: TUI, WebUI, CLI, any future surface (ACP adapter would sit here too).
- **Promise**: **surfaces are clients.** A surface's only internal dependencies are `client`-role
  crates (`chat-client-contract`, `liberado-commands`, `markdown`, `theme`) — enforced by
  `layer_rules.rs`. A deleted `ChatClient` trait (2026-07-05) is the cautionary tale for
  over-abstracting this seam; the decoder + DTOs are the real boundary.
- **Converged (2026-07-11)**: chat and goal-session streams share **one** event vocabulary —
  `wire::SessionEvent`/`SessionEventKind`, decoded by one `from_sse_data` for both streams. The
  executor's in-process `AgentEvent` is mapped onto it at the server boundary, mirroring the
  coding pack's `CoderEvent` → kernel `SessionEvent` mapping; the old chat-only `ChatEvent` is
  gone. SSE names are the kind's serde tags (`session`/`token` ride as bare payloads; `failed`
  not `error`, since browser `EventSource` reserves `error`).
- **Blast radius**: every surface at once. Version additively (new SSE event kinds are ignored by
  old clients).

## MCP + WriteProvenance

- **Defined**: MCP protocol via Turbomcp; Liberado's addition is `WriteProvenance`
  (`source` + `correlation_id`) riding request `_meta` into Turbovault's audit log, hash-joined
  back by the daemon's `attribute()` (Decision 5 — the loop-break).
- **Promise**: MCPs and hooks are the trust boundary — how agents touch real data, and the first
  point of user control. The `_meta` pass-through is the one upstream patch the whole provenance
  loop depends on (tracked for upstreaming; until merged, the local `[patch.crates-io]` pin is
  load-bearing).
- **Blast radius**: provenance/loop-breaking end-to-end (`vault/tests/provenance_e2e.rs` is the
  canary).

## CapabilitySet narrowing

- **Defined**: `liberado-common` (Decision 4). Not a trait — an invariant: authority only shrinks
  down a chain (`dispatcher_ceiling ∩ allowed_mcps` for subagents; pool = `component` key in
  `policy.toml`; proposals carry their pool as a signed field).
- **Promise**: self-extension cannot widen authority. This is the one property the reference
  systems (OpenClaw/Hermes-class) cannot retrofit, and no code path may violate it — the LLM
  proposes, deterministic code disposes, only ever toward less autonomy.
- **Blast radius**: this is the security model. Changes here get an eval (`liberado-eval`'s
  UNSAFE-acts must never increase) and a hardening-audit entry, not just a review.
