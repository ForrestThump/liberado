# Liberado — Roadmap

**What is not done yet.** Forward-looking work only. What *is* built is described in [`../architecture/overview.md`](../architecture/overview.md); finished plans and closed audits are in [`archive/`](archive/README.md). Index of this folder: [`README.md`](README.md).

Before starting anything, read [`../architecture/failure-modes.md`](../architecture/failure-modes.md) — five bug classes this codebase produces over and over. Every one of them shipped with a green test suite.

## Open now — in priority order

The order is deliberate: **automation daemon → chat → coding.** Why: [`../architecture/positioning.md`](../architecture/positioning.md).

### Priority 1 — the autonomous life-OS daemon

*Already in hand:* TurboVault storage + live plugins (vector + tasks); cron; Telegram free-form sticky chat + cron delivery; **C1 interactive crons (AskHuman)**; capability boundary; OpenClaw briefing cutover with `Succeeded` briefs; **MCP connection pooling (M1)** default-on; **M1b hot-reload**; **degraded-catalog routing**; **Tier-1 live conformance L1–L10**; **proposal expiry reaper**; **vault path-traversal guard**.

| # | What | Why it matters |
|---|---|---|
| **Dogfood** | **Lean on Telegram harder** | Collect friction → fix real pain. Free-form sticky chat is the phone surface. |
| **T1** | **Live conformance suite** — [`live-conformance-suite.md`](live-conformance-suite.md) | **L1–L10 landed.** **Open:** Tier 2 only. |
| **W1** | **Goal-session view in mobile WebUI** | Later phone surface beyond Telegram. See [`../architecture/session-surface-contract.md`](../architecture/session-surface-contract.md). |
| **E5-b** | ~~Telegram session deep-link~~ | **Deprioritized** (prefer WebUI later). |

### Priority 2 — lean chat surface

| # | What | Why |
|---|---|---|
| **CH1** | WebUI chat maturity | Model switch, daily usable chat beyond session view |
| **CH2** | Chat history search Tier 1 | [`chat-search-plan.md`](chat-search-plan.md) |
| **CH3** | Context compaction system | Summarize/compress long conversations to stay under model context windows without losing key facts |

### Priority 3 — coding pack (integration parity, not best-in-class)

| # | What | Why |
|---|---|---|
| **E6-c(b)** | Resume mid-build coding session | Design pass (git suspend point) |
| — | [`pr-dispatch-vtcode-no-write-finding.md`](pr-dispatch-vtcode-no-write-finding.md) | Open bug |
| — | [`coder-eval-curriculum.md`](coder-eval-curriculum.md) | After P1/P2 not bottleneck |

### Cross-cutting

- **External dependency audit** — audit all `Cargo.toml` entries across crates for unnecessary duplication, unused deps, version drift, and opportunities to share/slim. Goal: reduce compile wall-clock without breaking anything.
- **Modularity** remains the enabler: [`../architecture/modularity.md`](../architecture/modularity.md). Hot-path **module splits** landed (server API, daemon, config-loader model, executor budget).
- **A4 dual-store hub tests** (2026-07-23): list / cancel / park→resume / rehydrate via real `GoalSessionHub` on production `SessionStore` — `crates/session-store/tests/hub_dual_store.rs` (see [`../architecture/failure-modes.md`](../architecture/failure-modes.md) §1).
- **TurboVault modules**: vector + tasks paying back; remaining **`vault_events`** and upstream merge. Umbrella: [`turbovault-modules-integration-roadmap.md`](turbovault-modules-integration-roadmap.md).
- **Move-on bar:** leave P1 when you daily-drive without wincing — not when polished.

## What's next (one screen)

```
  P1 daily-driver ──►  dogfood Telegram
                   ├── C1 done (interactive crons → AskHuman via session profiles)
                   ├── M1b done (pool + degraded routing + topology MCP hot-reload)
                   └── T1 Tier-1 done (Tier 2 optional)

  Later ──► W1 mobile WebUI session view
  TurboVault (parallel) ──► vault_events · upstream land
```

## Recently landed

| When | What |
|------|------|
| **2026-07-23** | **C1 interactive crons (AskHuman):** profile-narrowed session grants — a cron schedule naming a `[[session_profiles]]` entry whose component includes `AskHuman` gets an open input channel; unattended crons stay structurally non-interactive (D-d). |
| **2026-07-23** | **M1b hot-reload** (`apply_mcp_peer_set` / `POST /api/mcp/reload`); **lock-poisoning recovery** (sessions + catalog); **proposal expiry reaper** (configurable, background); **vault path-traversal validation** (cross-platform); **MCP test double dedup** |
| **2026-07-23** | **Architecture hardening:** god-module splits; **MCP pooling** (M1); **M1b degraded-catalog routing**; **T1** L1–L10; **A4** dual-store hub tests |
| **2026-07-19** | TurboVault plugins live (vector + tasks); Telegram dogfood baseline |
| **2026-07-18** | Cron → Telegram delivery; OpenClaw brief cutover; sticky session persistence |
| **Earlier** | Unified Session (D7); one execution engine; `Write` at MCP boundary (F1); etc. |

See [`../architecture/sessions.md`](../architecture/sessions.md) for the session model history pointers.

**Last updated:** 2026-07-23.
