# Architecture alignment audit — 2026-07-11

**Scope**: the whole workspace (38 crates) + canonical docs, against the stated goals: Rust-first,
loosely coupled, config-driven, heuristically tuned, general-purpose agentic scaffolding that can
replace OpenClaw/Hermes-class assistants, LibreChat-class chat UIs, and Claude-Code-class coding
harnesses without rewrite regret.  
**Method**: dependency graph extracted from every `Cargo.toml` (real vs dev-deps separated), doc
claims spot-verified against code (`/api/goals*`, `liberado-session`, coder-agent module split,
executor coupling), prior audits cross-checked.  
**Companion**: [`agentic-mesh-hygiene-audit-2026-07-10.md`](agentic-mesh-hygiene-audit-2026-07-10.md)
(coder-pack-focused; its follow-up section records action-item statuses).

---

## Verdict 1 — Hand-rolled, loosely coupled Rust crates: **right call, and it's working**

This is not an aspiration; it's measurable in the graph:

- **The compile-time graph is a clean DAG.** No cycles. The two scary-looking edges
  (`common → config-loader`, `coder-agent → provider-openai-compat`) are dev-dependencies only.
- **Dev-dep discipline is real.** Live/telegram/provider-specific code reachable from libraries only
  through traits (`Notifier`, `Provider`); concrete impls appear in composition roots and
  `#[ignore]`d live tests.
- **The "could someone use just this crate?" test passes at the leaves**: `markdown`, `theme`,
  `chat-client-contract`, `conversation-store`, `session`, `provider` are genuinely liftable.
- **Restraint has been exercised**, which is the hard half of loose coupling: the `ChatClient` trait
  was deleted when it didn't earn its keep; session-type extraction was deferred until a consumer
  existed; the agent-pools research was commissioned *before* building peer coordination and its
  negative result was respected.

The cost being paid is the honest one: a solo-maintained 38-crate workspace, multi-model
development, and doc drift (see Verdict 4). Those are managed, not eliminated.

**One real layering leak found** (the only non-dev violation in the graph):

> `config-loader → coder-core` — `VerifierSpec`/`PipelinePolicy` imported to parse `[coder]`
> config. Since everything sits on `config-loader`, the coding pack's contract crate is now under
> the whole system. This is exactly the extraction trigger `modularity.md` defined, fired one layer
> lower than predicted. **Action**: lift neutral verify/intake DTOs into `liberado-common` (or a
> thin `liberado-verify`); `coder-core` keeps only `git_*` verifiers. Small, mechanical, high
> leverage.

Minor, acceptable-with-eyes-open couplings (documented so they stay deliberate):

| Edge | Assessment |
|---|---|
| `executor → notify` | Trait-only (`Notifier`) for risk-gated proposals. Fine; if `notify` ever grows heavy deps, move the trait to `common`. |
| `executor → scratchpad` | Deliberate (doom-loop mitigation as engine state). Fine. |
| `conversation-store → provider` | For `Message`/`Role` — the chat message vocabulary lives in `provider`. Acceptable; it's the narrow waist anyway. |
| `main-agent` (8 internal deps) | Widest library crate. It's a near-composition-root (the face agent composes dispatcher/orchestrator/mcp/store); watch it, don't fix it yet. |
| `heuristics-tuner → coder-agent` | Meta-tooling may see packs; it's not a build dep of the system. Fine. |

## Verdict 2 — "Mesh" is the wrong word; the architecture underneath it is right

The system is described as a mesh, but what is actually built — and what the project's own research
endorsed — is three different, *better-defined* shapes:

1. **Compile time: a layered DAG.** kernel (`common`, `provider`, `executor`, `session`,
   `orchestrator`) → packs (`coder-*`) → composition roots (`bootstrap`, `server`, binaries), with
   surfaces (`tui`, `webui`, `cli`) hanging off the wire contract, not off internals.
2. **Runtime: hub-and-spoke around one daemon.** One long-running `liberado serve` process; TUI /
   WebUI / CLI / Telegram attach as clients over HTTP/SSE; MCP servers attach as leaves over
   stdio/http/docker; cron/hooks/vault-watch feed one event channel. Pools deliberately never talk
   to each other.
3. **Agent topology: orchestrator + narrowed workers.** Dispatcher routes, capability sets only
   shrink, subagents report back. No peer-to-peer agent coordination — the
   `agent_pools_research_results.md` finding.

None of that is a mesh in the sense the word carries elsewhere (service meshes, mesh networks:
peer routing, any-to-any links, no privileged hub). Keeping the word has a real cost: it invites
exactly the two moves the project has already investigated and rejected — peer-agent coordination
and a big-bang event-bus rewrite (`meshify.md` step 5, now annotated). "Loose coupling" is the goal;
"mesh" is a topology claim, and it's not this topology.

**Recommendation**: keep "mesh" as informal branding if it has sentimental value, but make the
canonical vocabulary **"kernel + domain packs + surfaces, star topology around one daemon."**
Everywhere a doc says "mesh," it should be answerable which of the three shapes above it means.
(This audit does not rename existing docs; do it opportunistically as files are touched.)

The multi-process split (daemon, TUI, MCP forge, dispatch MCPs, deliberate-MCP talking over
HTTP/stdio) is correct and should stay: process boundaries are the security boundaries (capability
zones, Docker transport), and stdio/HTTP seams are what keep the pieces independently replaceable.
That's the part of the "mesh" instinct worth keeping — it just has a hub.

## Verdict 3 — Can one system beat OpenClaw, Hermes, LibreChat, and the coding TUIs?

Not on their home turf, and `positioning.md` already says so honestly: no chasing OpenClaw's 5,700
skills or LibreChat's enterprise multi-user UI. The winnable game is the one being played:
**provable containment + context efficiency + single-binary personal autonomy**, with the
capability-narrowing boundary as the one thing none of them can retrofit cheaply.

What this audit adds to positioning:

- **The generality bet is now evidenced, not asserted.** `LifeOpsDemoRunner` runs a non-coding goal
  session on the same kernel with no `coder-*` dependency; `/api/goals` accepts `domain: "life"` and
  `domain: "coding"` through one API. That is the structural claim OpenClaw/Hermes can't make.
- **The real existential risk is not capability, it's maintenance surface for one person.** Every
  competitor listed has a team or a community. The countermeasures that already exist —
  per-crate `ARCHITECTURE.md`, decision log, dated audits, `heuristics-tuner` as an automated QA
  force multiplier, model-agnostic dev workflow — are the right ones. The gaps are mechanical
  enforcement (below) and resisting surface sprawl: every new surface (Dioxus UI, Telegram, future
  ACP) must stay a *client of the wire contract*, which so far it has (`tui` depends on exactly
  `chat-client-contract` + `liberado-commands` + `markdown` + `theme` — nothing internal).
- **VTCode's failure mode is the cautionary tale to keep in view**: it collapsed under framework
  complexity (rig) rather than domain complexity. Liberado's equivalent temptation is kernel
  abstraction. The "second-domain test" and "extract only on real friction" rules are the antidote;
  they have been followed so far.

## Verdict 4 — Managing the complexity so it doesn't collapse

Ranked by leverage; 1–3 are cheap and concrete.

1. **Enforce layering mechanically, not by audit.** This audit found `config-loader → coder-core`
   by hand; the next leak should be caught by CI. Add a small workspace test (or `cargo-deny`-style
   check) that parses each `Cargo.toml` and asserts the layer rules: kernel crates import no
   `coder-*`; surfaces import no crates below the wire contract; only composition roots
   (`bootstrap`, `server`, `cli`, `heuristics-tuner`, `coder-runner`, `daemon`) may exceed N
   internal deps. ~100 lines, permanent payoff.
2. **Name the narrow waists as the architecture.** The system's stability comes from roughly nine
   frozen contracts: `Provider`, `ToolRuntime`, `EventSource`, `Notifier`, `ConversationStore`,
   `ConfigSource`, `DomainPackRunner`, the `Verifier` trait (landing), and the HTTP/SSE wire
   contract. Everything else is replaceable. A one-page `docs/spec/architecture/contracts.md` listing
   them, their stability promise, and who may implement them would do more for new-model onboarding
   than any prose — and gives every future "should X depend on Y?" question a fast answer: *if the
   dependency isn't one of the waists, justify it.*
3. **Generate the crate map.** `overview.md`'s table had drifted by six crates (fixed this pass).
   A tiny script emitting the table from `cargo metadata` + each crate's `description` field turns
   doc drift into a build artifact. (Prerequisite: a few crates' `description` fields need writing.)
4. **Keep the second-domain test ritualized.** Before promoting anything into kernel/`session`,
   demand the life-ops (or research) pack could use it unchanged. This is already design rule #10;
   the point is to keep applying it now that the session crate exists and will attract features.
5. **Cap the surfaces' ambition.** The TUI is the dogfood surface; the Dioxus UI is pre-alpha; ACP
   is an idea. Resist making any of them smart. All UI intelligence (slash commands, rendering,
   themes) is already in shared client crates — hold that line.
6. **Convergence debt worth scheduling** (from the 07-10 audit, still open): unify `AgentEvent`
   (chat) with `SessionEvent` (goals) into one envelope with domain payloads before the TUI grows a
   second event renderer. This is the highest-value *pending* unification; everything else can wait.

## Actions

| # | Action | Size | Status (2026-07-11 follow-up pass) |
|---|---|---|---|
| 1 | Drop `config-loader → coder-core` | S | ✅ done — by **inversion**, not extraction: `[tuning.coder]` is an opaque `toml::Value` in config-loader; `CoderTuning` (struct/defaults/validation) moved to `coder-core::tuning`, parsed via `CoderTuning::from_value` at composition time. Deeper than first scoped: the edge carried the whole coder config vocabulary, not just verify DTOs |
| 2 | Add the mechanical layer-rule test to CI | S | ✅ done — `crates/test-support/tests/layer_rules.rs` over `[package.metadata.liberado] role` tags on all 40 crates (pack containment, surface thinness, client/foundation purity, dep budget, mandatory tagging); runs in `cargo test --workspace`, wired into `.github/workflows/ci.yml` |
| 3 | Write `docs/spec/architecture/contracts.md` | S | ✅ done — 10-contract narrow-waist inventory; depth stays in crate ARCHITECTURE.mds |
| 4 | Generate the crate map | M | ✅ done — `scripts/gen-crate-map.ps1` → `docs/spec/reference/crate-map.md` from manifest `description` + `role` (same tags as the layer test, so graph and map cannot drift apart silently) |
| 5 | Converge `AgentEvent`/`SessionEvent` envelopes before more TUI session UI | M | ✅ done same-day — one wire `SessionEvent`/`SessionEventKind` in `chat-client-contract` decodes both `/api/chat/stream` and `/api/goals/{id}/stream`; kernel kind gained `Token`, `Error`→`Failed` (EventSource-safe tag); chat SSE renamed `tool`/`tool_result`/`done` → `tool_started`/`tool_finished`/`session_finished` with all in-repo clients moved atomically (`ChatEvent` deleted; CLI moved onto the typed decoder too). `AgentEvent` stays the executor's in-process tap, mapped at the server boundary like `CoderEvent` → `SessionEvent` |
| 6 | Vocabulary shift "mesh" → kernel · domain packs · stores · surfaces | ongoing | ✅ canonical docs done (overview vocabulary section, agentic-loops, modularity, positioning, verifiers, README, roadmap top); historical/dated docs keep the old word with a pointer |
| 7 | `.gitignore` the `tmp-liberado-*.log` files | XS | ✅ done + untracked |

Also landed in the follow-up pass: GitHub Actions CI (fmt + clippy `-D warnings` + full test
suite, ubuntu/windows matrix, sibling turbovault/turbomcp checkouts), workspace `cargo fmt`,
clippy cleanup to zero warnings (webui excluded from the gate — pre-alpha Dioxus scaffolding),
`rust-version` corrected to 1.91 (code already used 1.91-stable APIs).

**Doc fixes landed in this pass**: overview crate map (+6 crates, kernel line includes `session`),
modularity/verifiers/agentic-loops now record the fired extraction trigger, hygiene-audit follow-up
section, `meshify.md` audit annotation.
