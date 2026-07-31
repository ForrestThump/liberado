# TurboVault modules — integration roadmap (umbrella)

**Status**: **in progress — paying back live (2026-07-19).** Bird's-eye revamp triggered by the
TurboVault plugin API landing ([PR #39](https://github.com/Epistates/turbovault/pull/39),
`turbovault-plugin-api`). Updated after Liberado's homelab Telegram instance successfully drove
**vector search and tasks** through the live TurboVault peer.
**Scope**: how Liberado targets the new plugin boundary across **three modules** while keeping the
daemon working, staying loosely coupled, and minimizing upstream maintainer friction.
**Relationship to other docs**: this is the *umbrella*. The vault-events vertical already has a deep
implementation plan — [`turbovault-vault-events-plugin-plan.md`](turbovault-vault-events-plugin-plan.md).
This doc sits above it, tracks **tasks** / **vector** status, defines shared conventions, and ties
into [`roadmap.md`](../roadmap.md).

> If this conflicts with a newer maintainer comment on #33/#34/#39, the maintainer comment wins —
> update this file.

---

## 0. TL;DR (read this if nothing else)

- **The plugin API is in hand and dogfooded.** Fork `develop` has `turbovault-plugin-api` plus live
  module wiring. Liberado on the homelab (Telegram + HTTP) reaches the TurboVault peer and uses
  **vector search and tasks** in real sessions — the modules roadmap is no longer paper.
- **Three modules, not one.** Each is a **compiled-in, default-off, feature-gated vertical**
  in the TurboVault binary:

  | Module | id | Status (2026-07-19) | Nature of the work |
  |---|---|---|---|
  | **tasks** | `tasks` | **Done on `feat/plugin-tasks`** (extraction + self-tuning + recurrence + live tests); not yet on fork `develop` / upstream. Core task tools still on `develop` and already used in Liberado briefs. | **Extraction** onto the plugin boundary |
  | **vector** | `vector` | **Prototype Phases 1–4 DONE on fork `develop`** (engine + #42 state dir + #43 change-feed helpers + module). **Live** on homelab with `--features vector`. Upstream split PRs still open. | **In-tree engine** (`turbovault-vector`) behind the facade — not `lqm-core` |
  | **vault-events** | `vault_events` | **Next / planned** — deep plan written; not started as a module | **Rebuild** of [#24](https://github.com/Epistates/turbovault/pull/24) on #39 |

- **These are "modules," not third-party plugins.** They compile into the host, **we** maintain them,
  and Nick **curates** (reviews/merges) them. Same mechanism as a plugin; different ownership story.
  See §1.
- **None of the three blocks Liberado P1.** The daemon already perceives the vault via its own watcher
  (the `L0` fallback). Modules are a **consolidation + capability + synergy** play. With vector + tasks
  live, Liberado's *remaining* P1 is the phone-grade interfacing loop (W1 / E5-b), not vault storage —
  see [`roadmap.md`](../roadmap.md). *Don't let module polish derail the daily-driver bar.*
- **Actual build order diverged productively:** scaffolding + **vector** landed on `develop` first
  (highest capability payoff once the engine existed); **tasks** completed as a clean extraction on
  `feat/plugin-tasks`. **Next module work: land tasks onto `develop`, then `vault_events`.**

---

## 1. The reframe: modules, not plugins

Nick's umbrella ([#34](https://github.com/Epistates/turbovault/issues/34)) frames these as "plugins"
("if it's a community plugin in Obsidian, it's a plugin in TurboVault"). Mechanically that's exactly
what we're building. But our **ownership model is different from a marketplace plugin**, and the
roadmap should be honest about it:

| Property | Third-party plugin (the #34 vision) | **Our modules** |
|---|---|---|
| Author | Anyone | **Us** (Liberado) |
| Merge/curation | — | **Nick curates** (reviews + merges into TurboVault) |
| Distribution | (future) dynamic/marketplace | **Compiled-in, feature-gated** — no dynamic load, no FFI |
| Coupling | Loose by necessity | **Loose by discipline** — depend on `turbovault-plugin-api` *only* |
| Maintenance burden on Nick | High if in core | **Near-zero** — feature default-off; core catalog byte-for-byte unchanged when off |

**Why this is the good-news story you called it:** the plugin boundary lets us get deep synergy with
TurboVault (shared CAS, shared event envelopes, one binary, one deploy) **without** asking the upstream
maintainer to own our verticals. We carry the code; he curates the boundary. That's maximum
extensibility with minimum friction — the exact shape we wanted.

**The invariants that make "loose coupling by discipline" real** (every module, every PR):

1. Depend on `turbovault-plugin-api` **only** for host capabilities — never a raw `VaultManager`,
   MCP server, or transport session.
2. **CAS writes only** through `VaultApi` (`CreateOnly` or `Match(version)`); blind overwrite is not
   exposed and must not be reintroduced.
3. **Local tool names only**; the host namespaces them as `<id>_<local>` (`tasks_list`,
   `vault_events_subscribe`, `vector_search`).
4. **Default-off feature** that enables `plugin-api`; the default binary pulls **no** module.
5. **Hooks are advisory** — lag / close / `ExternalOrUnknown` are first-class; never invent
   reliability the bus doesn't have.
6. Core tools stay flat and stable when your feature is off (assert it in a test).

(These are the vault-events plan's "rules of the road," §7 there, elevated to apply to all three.)

---

## 2. Bird's-eye map

```
                          ┌─────────────────────────────────────────────┐
                          │            TurboVault host binary            │
                          │  core catalog (flat tools, unchanged off)    │
                          │                                              │
  turbovault-plugin-api ──┤  feature plugin-api  (default OFF)           │
   VaultApi (CAS facade)  │   ├── feature tasks         → tasks_*        │
   HookBus (broadcast)    │   ├── feature vault-events  → vault_events_* │
   Plugin/Provider        │   └── feature vector        → vector_*       │
                          └───────────────┬──────────────────────────────┘
                                          │ (HookBus envelopes / MCP tools)
                                          ▼
                          ┌─────────────────────────────────────────────┐
                          │                 Liberado                     │
                          │  daemon perception = EventSource (Decision19)│
                          │  L0 local watcher  ← authoritative today     │
                          │  L1 optional: consume vault_events_* over MCP│
                          │  vector_* → context policy / chat-search T3  │
                          │  tasks_*  → life-OS todo surface             │
                          └─────────────────────────────────────────────┘
```

Two distinct "plugin" ideas that must not be confused (carried over from the vault-events plan §6):

| Layer | "plugin/module" means | Status |
|---|---|---|
| **TurboVault** | compiled-in MCP vertical in the TV binary | **this roadmap** |
| **Liberado mesh** | vault is the default perception plugin behind `EventSource` | **already done** (Decision 19) |

---

## 3. What #39 gives us (confirmed against the vendored crate)

Do **not** re-specify these; depend on them. (Full detail in the vault-events plan §3; this is the
confirmation that the *real* crate matches the sketch.)

- **`Plugin`** → `descriptor() -> PluginDescriptor{id,name,version,description}` +
  `build(PluginContext) -> Arc<dyn PluginProvider>`.
- **`PluginContext`** = `{ vault: VaultApi, hooks: HookBus }`. That is the entire capability surface a
  module gets. (Note: **no filesystem-watcher handle** — the vault-events module's H1/H2/H3 fork about
  how it observes external edits is still live; see that plan §5.)
- **`PluginProvider`** → `tools() -> Vec<Tool>` (local names) + `call_tool(name, args,
  PluginRequestContext)`.
- **`PluginRequestContext`** = `{ request_id, user_id?, session_id?, client_id?, metadata }` — curated;
  no raw transport.
- **`VaultApi`** (CAS facade) = `active_vault` / `list_notes` / `read_note` (returns opaque `version`) /
  `write_note` (mandatory `WritePrecondition::CreateOnly | Match(version)`). `VaultHost` trait is public
  so modules can supply fakes in tests.
- **`HookBus`** = bounded `tokio::broadcast` of `VaultEventEnvelope { sequence, observed_at_ms, vault,
  event, content_hash?, attribution }`; `HookEvent::{FileCreated,Modified,Deleted,Renamed,ResyncRequired}`;
  `EventAttribution::{Attributed(WriteProvenance), ExternalOrUnknown}`; `HookRecvError::Lagged{skipped}`
  ⇒ resync via `VaultApi`.
- **`validate_plugin_id`**: `[a-z][a-z0-9_]*`. `tasks`, `vault_events`, `vector` all valid.

**Host wiring: done on the fork.** `crates/plugins/`, `new_with_plugins`, and feature flags
(`plugin-api`, `vector`, and on the tasks branch `tasks`) are real. Remaining host work is
upstream curation of #42 / #43 shapes and merging the tasks feature onto `develop`.

---

## 4. Module 1 — `tasks` (extraction; **done on branch**)

**Status (2026-07-19):** **implemented on `feat/plugin-tasks`.** Liberado already drives vault tasks
in production briefs (core task tools on `develop`); the plugin module is the clean CAS / namespaced
home for that surface.

**Target crate**: `turbovault/crates/plugins/turbovault-plugin-tasks/`, `id = "tasks"`.

**Landed on the branch:**

1. Extraction of historical task tools onto `VaultApi` (CAS writes only).
2. Local tools → host names `tasks_list`, `tasks_overdue`, `tasks_tags`, `tasks_complete`,
   `tasks_update`, `tasks_delete`, `tasks_config`.
3. Self-tuning to Obsidian Tasks settings via `VaultApi::read_config` (emoji vs dataview, global
   filter) with heuristic fallback.
4. Recurrence spawn on complete; renderer owned by the module (parse stays in core).
5. Feature-matrix + fail-closed live vault tests.

**Remaining:**

- Merge onto fork `develop` and enable in the homelab TurboVault image when ready.
- Upstream curation PR (default-off feature; no core catalog change when off).

**Payback into Liberado**: **already realized** as the life-OS todo surface in Telegram/briefs.
Plugin merge is polish + ownership cleanliness, not a greenfield capability.

---

## 5. Module 2 — `vault_events` (rebuild; already deeply planned)

**This module has its own implementation plan** —
[`turbovault-vault-events-plugin-plan.md`](turbovault-vault-events-plugin-plan.md). Nothing here
supersedes it; the umbrella only situates it.

**One-line**: watcher → `HookBus` envelopes + model-facing `vault_events_*` pull tools (subscribe /
fetch / unsubscribe), best-effort delivery, best-effort attribution (fail-open), closing the open half
of [#33](https://github.com/Epistates/turbovault/issues/33).

**What the umbrella adds / adjusts**:

- **Reframe "plugin" → "module"** in that doc's language when it's next touched (curation model, §1
  here). Low priority; cosmetic.
- **Phase 0 is done locally** — the plugin-api crate is vendored. Skip the "merge/track #39" step for
  local prototyping; keep it only for the eventual upstream PR.
- **The one live host-capability fork (H1/H2/H3, watcher access)** is the module's hardest open
  question, because `PluginContext` gives **no** filesystem handle (confirmed §3). Resolve it there;
  it does not affect tasks or vector.
- **Liberado consumption stays `L0`** (local watcher authoritative) until an `L1` MCP adapter proves
  parity. This module does **not** force a daemon change.

**Payback into Liberado**: consolidates perception onto an upstream-native path (optional `L1`), and
closes multi-agent attribution. Highest *architectural* synergy of the three; not a capability the
daemon lacks today.

---

## 6. Module 3 — `vector` (**prototype live**)

**Status (2026-07-19):** **prototype Phases 1–4 DONE on fork `develop` and live on the homelab.**
Liberado (Telegram) can call `vector_search` / related tools against the real vault. Detail and
goal conditions: [`archive/turbovault-vector-prototype-plan.md`](archive/turbovault-vector-prototype-plan.md);
architecture decisions: [`archive/turbovault-vector-module-plan.md`](archive/turbovault-vector-module-plan.md).

**What we actually built** (decision flipped from the original "depend on `lqm-core`" lean):

- In-tree **`turbovault-vector`** engine (fastembed + usearch + SQLite; content-fed
  `update_note` / chunk-level incremental) — same family life-os `memory-store` already used.
- **No `lqm-core` dependency** (would force life-os public); optional idea ports only.
- Host capabilities prototyped: **`plugin_state_dir` (#42)**, **`list_notes_meta` + watcher→HookBus
  bridge (#43)**, plus `read_config`.
- Module tools: `vector_search`, `vector_reindex`, `vector_status`, `vector_config`.
- Freshness: **mtime reconcile on demand** (before search / on reindex) via `list_notes_meta` →
  `update_note` — not a permanent HookBus subscription task yet.
- Live bugfix during dogfood: char-boundary-safe chunk overlap.

**Remaining for this module:**

- Upstream-ready **split PRs** for #42, #43, and the module (prototype branch is proof, not the
  merge unit).
- Graduate freshness toward HookBus / `vault_events` once that module exists (fork V-C).
- Multi-vault handle lifecycle and production hardening marked `// prototype:` in places.

**Payback into Liberado**: **realized now** — semantic vault search from the daily-driver agent.
Still serves future CH2 Tier 3 and context-policy retrieval without a bespoke Liberado index.

---

## 7. Sequencing — plan vs what happened

**Original recommendation:** `tasks → vault_events → vector` (scaffolding first, capability last).

**What shipped:** scaffolding + **vector** on fork `develop` (engine already existed; highest
dogfood payoff), **tasks** fully extracted on `feat/plugin-tasks`, **vault_events** still planned.

**Forward order now:**

1. **Merge `tasks` → fork `develop`** and optionally enable in the homelab image (capability already
   present via core tools; this is ownership + CAS module surface).
2. **Upstream curation** for #42 / #43 / vector (and tasks) as **split** reviewable PRs — prototype
   is proof, not the merge unit.
3. **`vault_events` next as a module** — plan is done; consolidates perception and unlocks better
   vector freshness. Liberado stays on L0 until L1 parity is proven.
4. **Vector hardening** (HookBus freshness, multi-vault, strip `// prototype:`) after or alongside
   vault_events.

**Per-module detail**: vault-events → its own plan. tasks → §4. vector → §6 + prototype plan. Never
bundle two modules in one PR series.

---

## 8. How this reshapes the Liberado roadmap (`roadmap.md`)

The plugin API does **not** change Liberado's top-line strategy (**daemon → chat → coding**; get one
thing over the daily-driver line). Modules already paid back; remaining Liberado P1 is **W1 / E5-b
interfacing**, not vault storage. Concretely:

| `roadmap.md` item | Effect of the modules |
|---|---|
| **P1 daily-driver (W1, E5-b, T1, M1)** | **Still the front of the queue.** Modules are live capability, not a reason to delay the session WebUI. |
| **P1 perception (vault watcher)** | `vault_events` still a future `L1` path; **`L0` stays authoritative**. |
| **CH2 — chat history search** ([`chat-search-plan.md`](chat-search-plan.md)) | Tier 3 (vector) is **subsumed by the `vector` module**. Keep Tier 1 (ripgrep) as planned. |
| **Context policy** ([`liberado-context-policy-spec.md`](../spec/context-policy-spec.md)) | Real retrieval backend via live `vector_*`. |
| **Agent memory / `memory-mcp`, `memory-store`** | Still evaluate overlap with standalone qdrant / memories before consolidating. |
| **Life-OS todos** | **Live** — briefs and chat drive tasks through TurboVault. |
| **Nice-to-have: A2A / mesh** | Unchanged. |

**Wired into [`roadmap.md`](../roadmap.md)** (2026-07-19): cross-cutting TurboVault modules line +
"Recently landed" entry for live vector/tasks + "What's next" one-screen summary.

---

## 9. Shared concerns (all three modules)

- **When to extract a shared `turbovault-plugin-common` crate**: **not yet.** Wait until 2–3 modules
  show *real* overlap (matches #34). tasks and vault-events will reveal whether the test scaffolding /
  CAS-write helpers are worth sharing; extract then, not speculatively.
- **Dependency hygiene**: `vector` is the one that pulls weight (Qdrant, embedders, `aws-lc-sys`).
  Keep each module's heavy deps behind *its* feature so a `--features tasks` build stays light. The TV
  `--all-features` / CI build must still compile — budget for the vector toolchain there (fork V-B).
- **Provenance alignment** (from vault-events plan §6): Liberado's `WriteProvenance` has an extra
  `zone` field the plugin-api lacks. Map `zone` → `note`/metadata at the boundary; don't block on it;
  propose an additive upstream field only if multiple modules need it.
- **Safety checklist per PR** (vault-events plan §7): blind overwrite impossible; no manager/session
  leakage; duplicate id/tool rejected at boot; attribution/hooks fail open; default features don't pull
  the module; docs give enable-instructions + non-goals.

---

## 10. Decision log (cross-module forks)

Extends the vault-events plan's F1–F12 with module-umbrella-level calls.

| ID | Fork | Prefer | Avoid |
|---|---|---|---|
| **M-1** | Build order | ~~`tasks → vault_events → vector`~~ → **as shipped: vector+scaffolding on develop, tasks on branch; next vault_events** | Starting net-new modules before dogfood |
| **M-2** | Ownership framing | "Modules we maintain, Nick curates" | Positioning as core TV features |
| **M-3** | Shared helper crate | Defer until 2–3 modules overlap | Speculative `plugin-common` up front |
| **M-4** | tasks: vault access | Route all reads/writes through `VaultApi` (CAS) — **done on branch** | Keeping any internal-manager or blind-write path |
| **M-5** | vector: engine | **In-tree `turbovault-vector`** (locked; no `lqm-core` dep) | Forking life-os / forcing public lqm |
| **M-6** | vector: freshness | **mtime reconcile-on-demand first** (shipped); graduate to `vault_events` envelopes | Growing a second private watcher inside vector |
| **M-7** | vector: isolation | Single active vault in prototype; multi-vault later | A parallel isolation model |
| **M-8** | Liberado consumption | `L0` authoritative; modules optional — **vector/tasks already consumed live** | Rewriting the daemon around a module before parity is proven |
| **M-9** | Scaffolding | `crates/plugins/` + feature pattern — **landed with vector on develop** | Re-inventing wiring per module |

---

## 11. Open questions (need a human call)

1. ~~**Reprioritize `roadmap.md`?**~~ **Done 2026-07-19** — pointer + cross-cutting + what's-next.
2. **`vector` vs existing `memory-mcp`/`memory-store` / `liberado-qdrant-mcp`** — consolidate, or keep
   separate surfaces (vault semantic search vs agent memory corpus)? Homelab currently has both
   TurboVault vector and `liberado-qdrant-mcp`.
3. **Upstream cadence** — push each module to Epistates as it lands (curation-as-you-go), or stage on
   the fork and PR in a batch once the pattern is proven?
4. **vault-events H1/H2/H3 watcher-capability fork** — still the gating unknown for that module.
5. **Homelab image features** — keep shipping `--features vector` only, or also bake `tasks` once
   merged to `develop`?

---

*End of umbrella. Module-level detail lives in each module's section (tasks §4, vector §6) or its own
plan (vault-events). Keep this file about sequencing, ownership, and cross-module forks — not
line-by-line design.*
