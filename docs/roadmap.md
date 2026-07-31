# Liberado — Roadmap

**What is not done yet.** Future-looking work only. What *is* built is described in [`spec/architecture/overview.md`](spec/architecture/overview.md); finished plans and closed audits are in [`future-work/archive/`](future-work/archive/README.md). Future work index: [`future-work/README.md`](future-work/README.md).

Before starting anything, read [`spec/architecture/failure-modes.md`](spec/architecture/failure-modes.md) — six bug classes this codebase produces over and over. Operator knobs (and which are compiled in) are in [`spec/reference/tuning.md`](spec/reference/tuning.md). Every one of them shipped with a green test suite.

## Open now — in priority order

The order is deliberate: **automation daemon → chat → coding.** Why: [`spec/architecture/positioning.md`](spec/architecture/positioning.md).

### Priority 1 — the autonomous life-OS daemon

*Already in hand:* TurboVault storage + live plugins (vector + tasks); cron; Telegram free-form sticky chat + cron delivery; **C1 interactive crons (AskHuman)**; capability boundary; OpenClaw briefing cutover with `Succeeded` briefs; **MCP connection pooling (M1)** default-on; **M1b hot-reload**; **degraded-catalog routing**; **Tier-1 live conformance L1–L10**; **proposal expiry reaper**; **vault path-traversal guard**.

| # | What | Why it matters |
|---|---|---|
| **Dogfood** | **Lean on Telegram harder** | Collect friction → fix real pain. Free-form sticky chat is the phone surface. |
| **T1** | **Live conformance suite** — [`live-conformance-suite.md`](future-work/live-conformance-suite.md) | **L1–L10 landed.** **Open:** Tier 2 only. |
| **W1** | **Goal-session view in mobile WebUI** | Later phone surface beyond Telegram. See [`spec/architecture/session-surface-contract.md`](spec/architecture/session-surface-contract.md). |
| **E5-b** | ~~Telegram session deep-link~~ | **Deprioritized** (prefer WebUI later). |

### Priority 2 — lean chat surface

| # | What | Why |
|---|---|---|
| **CH1** | WebUI chat maturity | Daily usable chat beyond session view (history, UX) |
| **CH4** | **Mid-session / per-conversation model switching** | See below — not the same as process-wide hot-swap |

**CH4 — mid-session model switching (open)**

*What we already have (process-wide, not per chat):* `GET /api/models` + `POST /api/models/select` (and TUI `/model`, Telegram model select) call `Provider::set_model` on the shared face provider. That hot-swaps the **daemon-wide** active model for *subsequent* completions — no restart. Every conversation shares that one current model; there is no per-session binding.

*What we do not have yet:*

| Gap | Why it matters |
|-----|----------------|
| **Per-conversation model** | Switch model for *this* chat only; other chats keep theirs |
| **Sticky preference** | Persist chosen model on the conversation (or session header) across reloads / restarts |
| **Surface UX** | Explicit mid-chat “use model X for this thread” in WebUI (CH1) and consistent TUI/Telegram semantics |
| **Dependent re-resolve** | On switch: recompute CH3 compaction trigger (`trigger_pct` × new model’s `context_window` / per-model overrides), status `model_name`, any role display — today resolve is boot-time for compaction |
| **Role clarity** | Face vs dispatcher/subagent: mid-session switch is about the **chat face** unless we later add per-role runtime swap |

*Not a substitute:* boot-time `[roles.main_agent] model = "…"` in `topology.toml` (edit + restart). That is fixed wiring, not mid-session.

*Suggested acceptance (when scheduled):* pick model mid-chat → only that conversation’s next turns use it; other conversations unchanged; preference survives reopen; compaction/status track the active face model for that chat.

### Priority 3 — coding pack (integration parity, not best-in-class)

> **Pulled forward by owner decision (2026-07-24):** the agentic coding TUI track is now active
> alongside P1/P2 — plan: [`coding-tui-plan.md`](future-work/coding-tui-plan.md) (goal-driven TUI surface +
> kernel completion gate, Grok Build-style disputed-claim completion, slices S1–S7).

**Slice status** (plan: [`coding-tui-plan.md`](future-work/coding-tui-plan.md) §Slices):

| Slice | State | Notes |
|---|---|---|
| **S1** — completion gate | ✅ **landed** | `liberado_session::completion_gate` (gatekeeper veto + strict-majority fresh quorum + fail-closed votes), coding-pack adapter, `critic_verdict` on the wire, strategist on non-convergence. **Default OFF** (`[coder.gate] enabled`) — it costs `1 + fresh_reviewers` model calls per attempt, and stays opt-in until S7 measures it. |
| **S2** — wire events + goal surface | 🟡 **partial** | Done: `file_changed`, first-class `hub.park()` + `POST /api/goals/{id}/park`, `/goal` commands (`start`/`in`/`status`/`pause`/`resume`/`clear`), TUI wiring for all of them. **Not done:** dedicated goal-view panes (role timeline / gate panel / verifier panel as separate widgets — gate votes and file changes currently render inline in the joined pane), `GET /api/goals/{id}/diff`, and the live dogfood run. |
| **S3–S7** | ⬜ open | project authorization, checkpoints/rewind, `/loop`, coding subagents, strategist evals |

Two carried-forward limitations, both S2 leftovers worth knowing before building on this:

- **Gate votes reach the wire batched at attempt end, not live per vote.** The kernel's
  `GateObserver` supports live emission; `CoderBackend::run` has no `SessionEvent` sender to plumb
  it through. Wiring one is the remaining half of "watch the quorum vote".
- **No agent can fan out, and that is currently the only thing preventing a workspace race.**
  `dispatch_parallel` is built but unreachable; `delegate` is synchronous; the executor runs tool
  calls serially. `WorktreeWorkspace` does not exist yet, so isolation must land before any of those
  change — [`agentic-loops.md`](spec/architecture/agentic-loops.md) §Concurrency, design rule 11.
- **Compaction tail copies still exist on disk** (CH3.1 territory) — any *new* reader that walks a
  raw leaf path must skip `Author::is_compaction_tail_copy()`.

| # | What | Why |
|---|---|---|
| **CT1** | **Agentic coding TUI** — [`coding-tui-plan.md`](future-work/coding-tui-plan.md) | `/goal` + critic-gated completion + `/loop`, on the existing TUI/hub/coding pack; loosely coupled kernel machinery. S1 done, S2 partial (see above) |
| **E6-c(b)** | Resume mid-build coding session | Design pass (git suspend point) |
| — | [`pr-dispatch-vtcode-no-write-finding.md`](future-work/pr-dispatch-vtcode-no-write-finding.md) | Open bug |
| — | [`coder-eval-curriculum.md`](future-work/coder-eval-curriculum.md) | After P1/P2 not bottleneck |

### Cross-cutting

- **External dependency audit** — audit all `Cargo.toml` entries across crates for unnecessary duplication, unused deps, version drift, and opportunities to share/slim. Goal: reduce compile wall-clock without breaking anything.
- **Modularity** remains the enabler: [`spec/architecture/modularity.md`](spec/architecture/modularity.md). Hot-path **module splits** landed (server API, daemon, config-loader model, executor budget).
- **A4 dual-store hub tests** (2026-07-23): list / cancel / park→resume / rehydrate via real `GoalSessionHub` on production `SessionStore` — `crates/session-store/tests/hub_dual_store.rs` (see [`spec/architecture/failure-modes.md`](spec/architecture/failure-modes.md) §1).
- **TurboVault modules**: vector + tasks paying back; remaining **`vault_events`** and upstream merge. Umbrella: [`turbovault-modules-integration-roadmap.md`](future-work/turbovault-modules-integration-roadmap.md).
- **Redundant tool calls hidden by the doom-loop guard** (found 2026-07-28 in the passing
  `evening-debrief` live run, build `66b5771`). The subagent called `liberado-caldav-mcp:list_events`
  **four times for two dates** — twice on turn 2, twice again on turn 3 — before the guard fired
  (`doom loop detected; nudging once`), after which it recovered and filed on turn 4. The run
  **succeeded**, which is the point: the guard is currently absorbing a 2× redundancy rather than the
  redundancy being fixed, so it shows up as latency and spend, not as a failure. Worth a look when
  the executor's tool loop is next touched — the guard should stay, but it should be catching
  pathology, not routine duplication.
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
| **2026-07-23** | **CH3 context compaction:** long chats roll older history into a persisted summary marker (`Author::Named("compaction")` in the session DAG) — the model resumes from the summary + a verbatim tail of the last K user turns; the full transcript stays on disk, rendered and searchable. `[main_agent.compaction]` knobs, default on. Plan + 4-project research (OpenCode/Kilo/LibreChat/OpenClaw): [`context-compaction-plan.md`](future-work/context-compaction-plan.md). **Known residual** (marker durable + partial tail re-append → next load can miss unpersisted tail): documented there; preferred fix is CH3.1 viewport/side-summary — [`context-compaction-viewport-rearchitecture.md`](future-work/context-compaction-viewport-rearchitecture.md). |
| **2026-07-05** | **CH2 chat history search Tier 1** *(landed then, recorded now — the entry sat open here by mistake)*: lexical AND/regex over the session JSONL logs behind `GET /api/conversations/search`, the webui sidebar search box, and the `chat-search` MCP for the dispatcher. [`chat-search-plan.md`](future-work/chat-search-plan.md). |
| **2026-07-23** | **C1 interactive crons (AskHuman):** profile-narrowed session grants — a cron schedule naming a `[[session_profiles]]` entry whose component includes `AskHuman` gets an open input channel; unattended crons stay structurally non-interactive (D-d). |
| **2026-07-23** | **M1b hot-reload** (`apply_mcp_peer_set` / `POST /api/mcp/reload`); **lock-poisoning recovery** (sessions + catalog); **proposal expiry reaper** (configurable, background); **vault path-traversal validation** (cross-platform); **MCP test double dedup** |
| **2026-07-23** | **Architecture hardening:** god-module splits; **MCP pooling** (M1); **M1b degraded-catalog routing**; **T1** L1–L10; **A4** dual-store hub tests |
| **2026-07-19** | TurboVault plugins live (vector + tasks); Telegram dogfood baseline |
| **2026-07-18** | Cron → Telegram delivery; OpenClaw brief cutover; sticky session persistence |
| **Earlier** | Unified Session (D7); one execution engine; `Write` at MCP boundary (F1); etc. |

See [`spec/architecture/sessions.md`](spec/architecture/sessions.md) for the session model history pointers.

**Last updated:** 2026-07-24.
