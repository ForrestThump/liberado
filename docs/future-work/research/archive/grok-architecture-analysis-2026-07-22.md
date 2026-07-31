# Grok's architecture analysis â€” Liberado (life-os)

**Author**: Grok (xAI)  
**Date**: 2026-07-22  
**Branch context**: After merging `prototype/turbovault-plugin-integration` into `main`; working tip `dev/post-turbovault-merge` at `1d082f5`.  
**Scope**: Critical review of Liberado's existing architecture and code shape â€” not a rewrite plan. Recommendations are ordered by leverage against the stated strategy (**daemon â†’ chat â†’ coding**).  
**Primary sources**: `docs/architecture/{overview,contracts,modularity,failure-modes,positioning}.md`, `docs/roadmap/current.md`, `docs/handoff.md`, crate map, `layer_rules.rs`, hot-path crates (`server`, `daemon`, `mcp`, `executor`, `bootstrap`, `config-loader`, `messaging`).

---

## 0. Frame

Liberado is a Rust-native personal AI Life OS: one daemon watches a vault, reasons with an LLM, and acts through MCP tools under capability/zone containment â€” without reacting to its own writes (provenance loop-break). Surfaces (TUI, WebUI, CLI, Telegram) are clients; they do not own the loop.

This note asks:

> Where is the architecture load-bearing and correct, where is it stressed, and what improvements pay back the most without abandoning the sequencing strategy?

It does **not** recommend a peer agent mesh, absorbing every MCP into the workspace, or gold-plating P1 forever.

---

## 1. Executive summary

| Rank | Finding | Severity | Suggested move |
|---|---|---|---|
| **1** | MCP connections are **fresh per execution** (no pool) | High for unattended life-ops | M1: pool/reuse + degraded-catalog state |
| **2** | Telegram + sticky chat live inside the **composition root** | High for multi-channel | Finish `liberado-messaging` migration; thin server |
| **3** | Chat tests can still exercise **non-production store** paths | High (known false confidence) | Conformance only on `SessionStore` |
| **4** | `config-loader` model is a **god module** (~2k lines) | Medium velocity | Split by topology/policy/tuning sections |
| **5** | `liberado-common` holds **runtime state** (live catalog) under a "pure types" claim | Medium layering | Lift catalog out when next friction hits |
| **6** | Nested MCP / TurboVault fleet has **no single pin file** | Medium ops | Committed fleet lock of git revs |
| **7** | Hot-path god files (`daemon`, `executor`, `api`) | Medium maintainability | Split by lifecycle phase |
| **8** | Latency instrumentation not yet **closed-loop** on budgets | Medium reliability | Budget policy from measured p95 |
| **9** | Documented security holes (no FS sandbox on MCP children; HTTP no bearer; proposal writer-identity) | Structural | Pick one LAN policy; track OS sandbox as constraint |
| **10** | Docs/plans volume >> live scoreboard | Soft | One ops status table; archive the rest |

**Bottom line:** Seams are strong; edges (MCP lifecycle, surface glue, test/production duals, config bulk) are where daily-driver friction lives. Highest ROI: **M1 pooling**, **kill dual store tests**, **finish messaging extraction**.

---

## 2. What is working well (keep this)

### 2.1 Narrow-waist contracts are real

Frozen seams in `docs/architecture/contracts.md` â€” `Provider`, `ToolRuntime`/`RuntimeFactory`, `EventSource`, `DomainPackRunner`, dual session lenses, HTTP/SSE wire contract, `CapabilitySet` narrowing, MCP + `WriteProvenance` â€” are not slogans. `crates/test-support/tests/layer_rules.rs` mechanically enforces pack containment, surface thinness, client purity, foundation purity, and dependency budgets.

### 2.2 Safety is engineered, not prompted

Downgrade-only dispatcher guards, provenance loop-break (Decision 5), zone/write gating fail-closed at boot, signed proposal `pool` fields, session grants â€” this is the differentiator vs OpenClaw/Hermes-class systems. Self-extension cannot widen authority in the capability model.

### 2.3 Daemon-first star topology

One daemon; surfaces and MCPs attach; pools do not peer-coordinate. Agent-pools research correctly rejected any-to-any agent mesh. One `GoalSessionHub` + dispatch pack as a `DomainPackRunner` is coherent with "one execution engine."

### 2.4 Failure-modes doctrine

`docs/architecture/failure-modes.md` is institutional memory worth more than most audits:

1. Test pointed at the wrong object  
2. Guard that was off by default  
3. Narration outran the code  
4. Machine check that could overrule the human  
5. Write-only memory seams  

Every PR review should still use this checklist.

### 2.5 Messaging extraction direction

`liberado-messaging` (`MessagingChannel`, `ChatSurface`, `ActionButton`, `InboundEvent`) is the right abstraction for multi-channel without forking Telegram glue forever. Migration is incomplete (see Â§3.2).

### 2.6 Live proof on homelab

As of 2026-07-19 handoff: Telegram sticky chat + cron delivery, TurboVault vector + tasks, OpenClaw briefing cutover with `Succeeded` briefs. Strategy and production reality are aligned enough to dogfood.

---

## 3. Critical issues (detail)

### 3.1 MCP connection lifecycle is the P1 reliability tax

`liberado-mcp`'s registry opens a **fresh connection per execution** (documented in `crates/mcp/ARCHITECTURE.md` and `factory.rs`). Life-ops means many short sessions (cron, Telegram turns, delegated subagents). Cost:

- Cold handshake latency on every brief  
- Peer flakiness (weather/CalDAV-class) amplified  
- No health cache / circuit breaker across turns  

**Recommendation:** Treat M1 as reliability, not ops polish.

- Pool keyed by `(mcp_name, transport)` with idle TTL + reconnect-on-error  
- Optional boot-time warm connect for MCPs marked critical in topology  
- Degraded state in the capability catalog so the dispatcher can avoid routing to a dead peer instead of burning turn budget  

Without this, live conformance (T1) will keep rediscovering intermittent tool failures.

### 3.2 Composition root is becoming a second product surface

`liberado-server` owns HTTP/SSE (`api.rs` ~1.5k lines), Telegram free-form + sticky session, cron delivery folding, latency journal, and pack registration. Telegram is the primary phone surface but is implemented as a server module, not a thin adapter behind `MessagingChannel` / `ChatSurface`.

| Move | Into |
|---|---|
| Sticky session + free-form bridge | messaging adapter or `telegram-surface` crate |
| Approval button handling | finish split into `telegram-approvals` |
| Keep in server | wire routing, hub assembly, `AppState` |

A second channel (Matrix idea) should be a new adapter, not a second `telegram.rs`.

### 3.3 Test / production dual implementations still lurk

`failure-modes.md` and `contracts.md` still flag chat tests on `JsonlStore` while production uses `SessionStore`. That class already hid real defects.

**Hard rule:**

- Conformance and chat tests construct **only** production types  
- Quarantine or delete pre-convergence doubles  
- Prefer a CI check / mutant run that would fail if cancel/list become no-ops (already learned on session kernel)

### 3.4 `config-loader` model is a god module

`crates/config-loader/src/model.rs` is ~2k lines of TOML-shaped types. Everything boots through it; every feature tends to add another struct.

**Recommendation:** Split by config *section* (topology / policy / tuning / schedules+hooks), keep `ChainLoader` thin, keep pack sections opaque (`toml::Value` + pack-owned parse) â€” that inversion already prevented coding-pack leakage into the config stack.

### 3.5 `liberado-common` claims purity it no longer has

Docs: pure data, no logic, no I/O, no async. Reality: live `CapabilityCatalog` with `Arc<RwLock<_>>` + watch channel, session grants, guidance helpers, local-time stamping.

**Incremental fix:** lift live catalog to something like `liberado-catalog` when next friction forces a touch; keep pure vocabulary (provenance, capability, dispatch DTOs, event payload, proposal shapes) in `common`. Do not big-bang rewrite.

### 3.6 Nested MCP fleet without monorepo tooling

Pattern: gitignored sibling checkouts (`liberado-*-mcp/`, `turbovault/`, `turbomcp/`) + path deps + homelab builds from GitHub URLs. Correct for co-dev; fragile for "what revision is live?"

**Recommendation:** a committed fleet pin file (git rev per peer), consumed by deploy scripts and diagnosis docs. Do **not** force all MCPs into the Liberado Cargo workspace.

### 3.7 Hot-path god files

| Hotspot | ~Lines | Risk |
|---|---|---|
| `daemon/src/lib.rs` | ~2500 | react + attribute + proposal + pools |
| `executor/src/lib.rs` | ~2400 | loop + doom + budgets |
| `server/src/api.rs` | ~1500 | entire surface API |
| `tui` crate | ~8900 | OK if surface-isolated |

Split by **lifecycle phase** (watch / react / proposals / pools; loop / doom / budget / report; API route groups matching `docs/reference/api.md`). Contracts already allow it.

### 3.8 Latency instrumented, not closed-loop

Role-tiered models + JSONL latency journal + dispatcher on flash are good. Missing: policy from measurement â€” turn/token budgets matching measured brief cost; optional turn-budget "battery" for unattended runs. Without that, `PartiallySucceeded` from budget burn returns when tools flap.

### 3.9 Security boundary â€” known open holes

| Gap | Note |
|---|---|
| MCP child process has no FS sandbox | Capability model is logical, not OS-enforced |
| HTTP MCP client has no bearer injection | Token-gated peers awkward on LAN |
| Proposal writer-identity verification | Approval trusts content, not process identity |
| Docker MCP transport no live smoke | Isolation story unproven |

Pick **one** next hard boundary: LAN-unauth + network policy **or** optional HTTP bearer. Track OS process sandbox as a structural constraint, not a near-term pretend TODO.

### 3.10 Docs volume vs execution signal

Architecture writing is excellent; the ratio of plans/ideas/archive to "is the live daemon happy today?" is high. Dogfood is already the best detector.

**Recommendation:** one operational scoreboard (date, morning brief Succeeded?, MCP connect set, sticky restart) in handoff/goal. Archive plans that do not drive the next commit.

---

## 4. Suggested improvement roadmap

Compatible with P1 order in `docs/roadmap/current.md`.

| When | Improvement | Why |
|---|---|---|
| **Now** | MCP pool/reuse + degraded catalog | Cuts primary unattended failure mode |
| **Now** | Point all chat store tests at `SessionStore` only | Closes known false-confidence class |
| **Next** | Finish messaging migration out of server | Unblocks Matrix/etc without rewrite |
| **Next** | Fleet pin file for MCP/TurboVault revs | Deploy truth in one place |
| **Next** | Split config-loader model + daemon modules | Velocity for subsequent features |
| **Soon** | Live conformance suite (T1) on real daemon | Codifies failure-modes doctrine |
| **Parallel** | HTTP bearer **or** deliberate LAN-unauth policy | Fleet security consistency |
| **Defer** | Coding pack polish, WebUI maturity | After daily-drive bar |

---

## 5. What not to do

1. **Peer agent mesh / A2A** â€” research already rejected; pools that do not talk is correct.  
2. **Big-bang rewrite of `common`** â€” extract catalog when needed.  
3. **Absorb all MCPs into the Liberado workspace** â€” path-dep siblings + gitignore is fine; pin revs.  
4. **Telegram session multiplexing** â€” deprioritized; sticky chat + later mobile WebUI.  
5. **New abstraction traits without a second consumer** â€” `ChatClient` deletion is the cautionary tale.  
6. **Gold-plate P1 forever** â€” move-on bar is "daily-drive without wincing," not polish.

---

## 6. Crate / layer snapshot (for orientation)

Layer vocabulary: **foundation â†’ kernel â†’ stores / packs â†’ services / surfaces â†’ composition roots**. Generated inventory: `docs/reference/crate-map.md`.

Largest source trees observed (approx line counts, 2026-07-22): tui ~9k, heuristics-tuner ~5.6k, coder-agent ~5k, server ~3.6k, executor ~3.6k, config-loader ~2.9k, session ~2.8k, daemon ~2.7k, common ~2.5k.

Composition roots that may see everything: `bootstrap`, `server`, `cli`, `daemon`. Surfaces must stay on client-role crates only.

---

## 7. Git hygiene note (same session)

On 2026-07-22:

- `/liberado-python-interpreter-mcp/` added to root `.gitignore` (nested MCP pattern)  
- `prototype/turbovault-plugin-integration` fast-forwarded into `main`  
- Branch `dev/post-turbovault-merge` cut from updated `main`  
- `main` was **not** pushed to origin in that session (operator decision)

---

## 8. Related docs

- [`../architecture/overview.md`](../../../spec/architecture/overview.md) â€” cold-start map  
- [`../architecture/contracts.md`](../../../spec/architecture/contracts.md) â€” narrow waists  
- [`../architecture/failure-modes.md`](../../../spec/architecture/failure-modes.md) â€” five recurring bug classes  
- [`../architecture/modularity.md`](../../../spec/architecture/modularity.md) â€” seam plan  
- [`../architecture/positioning.md`](../../../spec/architecture/positioning.md) â€” replacement priority  
- [`../roadmap/current.md`](../../../roadmap.md) â€” open work order  
- [`../handoff.md`](../../../project/handoff.md) â€” live ops  
- [`../ideas/vs-grok-build.md`](../../ideas/vs-grok-build.md) â€” TUI coding gaps (separate product frame)

---

*End of Grok architecture analysis (2026-07-22).*
