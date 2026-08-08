# Liberado — Roadmap

**What is not done yet.** Future-looking work only. What *is* built is described in [`spec/architecture/overview.md`](spec/architecture/overview.md); finished plans and closed audits are in [`future-work/archive/`](future-work/archive/README.md). Future work index: [`future-work/README.md`](future-work/README.md).

Before starting anything, read [`spec/architecture/failure-modes.md`](spec/architecture/failure-modes.md) — six bug classes this codebase produces over and over. Operator knobs (and which are compiled in) are in [`spec/reference/tuning.md`](spec/reference/tuning.md). Every one of them shipped with a green test suite.

> **Picking up self-scoped work?** Start at [`future-work/backlog.md`](future-work/backlog.md) —
> one item per PR, verify it is still open before starting, and paste per-changed-behaviour mutation
> evidence in the PR body. This page is the *why*; the backlog is the *what next*.

## Open now — in priority order

The order is deliberate: **automation daemon → chat → coding.** Why: [`spec/architecture/positioning.md`](spec/architecture/positioning.md).

### Priority 1 — the autonomous life-OS daemon

*Already in hand:* TurboVault storage + live plugins (vector + tasks); cron; Telegram free-form sticky chat + cron delivery; **C1 interactive crons (AskHuman)**; capability boundary; **session profiles** (per-conversation authority: tools, delegation, model, prompt nudge); **approval ledger** (human decisions live outside the vault); OpenClaw briefing cutover with `Succeeded` briefs; **MCP connection pooling (M1)** default-on; **M1b hot-reload**; **degraded-catalog routing**; **Tier-1 live conformance L1–L10**; **proposal expiry reaper**; **vault path-traversal guard**.

| # | What | Why it matters |
|---|---|---|
| **Dogfood** | **Lean on Telegram harder** | Collect friction → fix real pain. Free-form sticky chat is the phone surface. |
| **T1** | **Live conformance suite** — [`live-conformance-suite.md`](future-work/live-conformance-suite.md) | **L1–L11 landed. Tier 3 P1a–P6 landed** (P6 = durable turn outlives its connection, PR #31, verified live). **P7 restart survival landed** (PR #36, passing live). Tier 2 remains optional. Note P5–P7 are all opt-in; a plain suite run is P1a–P4. |
| **W1** | **Goal-session view in mobile WebUI** | Later phone surface beyond Telegram. See [`spec/architecture/session-surface-contract.md`](spec/architecture/session-surface-contract.md). |
| **E5-b** | ~~Telegram session deep-link~~ | **Deprioritized** (prefer WebUI later). |

### Priority 1.5 — token economics (foundational, measured 2026-08-02)

The first real read of `liberado-cost` against the deployed journal (1,338 calls, 15.5M tokens) says
the spend is not where the architecture assumed. Full measurements, method and caveats:
[`token-economics-findings-2026-08.md`](future-work/token-economics-findings-2026-08.md).

**The finding in one line: 56% of every token ever spent is the orchestrator's ~11k base context,
re-sent on every hop of every run.** Face context — the thing delegation exists to protect — is
4.5%. The dispatcher is 2.8%.

| # | What | Why it matters |
|---|---|---|
| **TE1** | **Find out why the tool catalog isn't narrowed** | **56% of all spend.** Narrowing already exists (`relevant_mcps`, `allowed_mcps`, `narrow_direct_tools` defaults on) yet the base is a flat ~11k ≈ the full 12-MCP catalog. **Start by instrumenting, not fixing** — the `allowed_mcps` count is `debug`-level and the box runs `info`, so the cause is currently unobservable. **Not delegated:** it is a diagnosis whose scope is unknown until the measurement returns, which is the one shape you cannot write acceptance criteria for. Instrument in-house, collect a day, *then* spec the fix. |
| **TE2** | **Split subagent vs direct-execution spend** | `AgentRole` has no `Subagent`, and the role is bound at provider construction ([`bootstrap`](../crates/bootstrap/src/lib.rs#L203)) — one instance serves both dispatch paths — so this is a design task, not an enum addition. Specced as [round 3 §2](future-work/parallel-deliverables-2026-08-round-3.md). |
| **TE3** | **Order the dispatcher prompt for cache reuse** | Dispatcher cache hit is 22.3% vs ~76% elsewhere, because the varying goal is formatted *before* the stable MCP catalog and poisons the prefix. One-line reorder. Only ~1% of tokens — listed because it is nearly free and the same mistake in the orchestrator's prompt would not be. |

Do them in that order. TE3 is the tempting one to start with because it is a format string; it is
also the one that matters least.

### Priority 2 — lean chat surface

| # | What | Why |
|---|---|---|
| **CH1** | WebUI chat maturity | Daily usable chat beyond session view (history, UX) |
| **CH4** | ~~Mid-session / per-conversation model switching~~ | **Mechanism landed** (2026-07-31, `bd4f67a`); WebUI, TUI, Telegram and the compaction trigger all closed (last gap closed 2026-08-02, PR #35). Kept below for the reasoning. |

**CH4 — mid-session model switching (complete; retained for the reasoning)**

*What we already have (process-wide, not per chat):* `GET /api/models` + `POST /api/models/select` (and TUI `/model`, Telegram model select) call `Provider::set_model` on the shared face provider. That hot-swaps the **daemon-wide** active model for *subsequent* completions — no restart.

*What landed (2026-07-31):* a session profile may name a `model`, and that binding is now honoured end to end. `TurnSettings.model` comes off the conversation header ([`sessions.rs:775`](../crates/main-agent/src/sessions.rs#L775)) and `Executor::with_model` specialises an executor **per turn** ([`sessions.rs:588`](../crates/main-agent/src/sessions.rs#L588)), so one conversation choosing a model cannot change it for anyone else. `CompletionRequest.model` is honoured at the wire and beats a hot-swapped provider default — covered by the provider seam tests (`provider-openai-compat`, `per_request_model`). Because the profile lives on the persisted header, the preference survives reload and restart. Five tracing spans that reported the *provider's* model — not the one the request used — were fixed in the same pass.

*What is still open:*

| Gap | Why it matters |
|-----|----------------|
| ~~**Surface UX**~~ | **Closed 2026-08-01 (WebUI), 2026-08-02 (TUI, PR #30).** `/model` binds a model to the open conversation. There is no stored "selected model": `MessageNode.model` records what each turn ran on and the next turn reads the last one back off the log, so a conversation stays where it was put without a second field to drift. A pick made before the first message rides `ChatRequest.model` on the request that creates the conversation. |
| ~~**Dependent re-resolve**~~ | **Closed 2026-08-02 (PR #29).** The compaction trigger is a function of the conversation's resolved model, evaluated per turn against a per-model table built from `[[models]]` windows. A daemon-wide face swap now moves only the default, so it cannot retune a conversation that pinned its own model. No CH3.1 rearchitecture was needed. |
| ~~**Telegram `/model`**~~ | **Closed 2026-08-02 (PR #35).** It had been calling `provider.set_model` and replying "Model switched" while the sticky conversation, having history, kept resolving via `model_last_used` — so the next turn ran on the *old* model and every other unpinned chat silently moved. It now scopes to the sticky conversation through `ChatSessions::select_model`, the same call WebUI and TUI make. Telegram also gained `/stop` and turn-lifecycle replies in the same PR. |

*Resolved by the above, no longer gaps:* per-conversation model, sticky preference, and role clarity (the binding is the **chat face**, by construction).

*Not a substitute:* boot-time `[roles.main_agent] model = "…"` in `topology.toml` (edit + restart). That is fixed wiring, not mid-session.

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
                   └── T1 Tier-1 done (Tier 3 open; Tier 2 optional)

  P1.5 token economics ──► TE1 instrument the tool catalog   (56%; in-house, diagnosis)
                       ├── TE2 split subagent vs direct       (round 3 §2, delegated)
                       └── TE3 dispatcher prompt order        (cheap, ~1%)

  Round 3 (delegated) ──► 1. delegated findings reach the face  (correctness)
                      ├── 2. subagent vs direct in the journal  (= TE2)
                      └── 3. executor accumulation term         (37.4%)

  Later ──► W1 mobile WebUI session view
  TurboVault (parallel) ──► vault_events · upstream land
```

## Recently landed

| When | What |
|------|------|
| **2026-08-03** | **Zone identity fix** (PR #38): a zone is identified by its *name*, not by which `Zone` variant spelled it. `Policy::write_class` always keyed on the name; `CapabilitySet` used derived structural equality, so a `Named` grant could never satisfy the write gate's `Zone::vault(..)` check — latent because nothing constructs `Named` yet, and waiting for the first non-vault CRUD surface. Serialization deliberately untouched (`Capability` is in `policy.toml` **and** the proposal HMAC). `tuning.md` gained the non-vault zone guide. **Seam boundary tests** (PR #39) and two read-only analysis tools: `delegation_cost.rs` and `provenance_ratio.rs` (examples then; promoted to `liberado-cost delegation-cost` / `provenance-ratio` in PR #63), which ranked the known seam conversation first at 29.4x against a median of 0.9x. **Evals decision** recorded in [`evals_implementation.md`](future-work/research/evals_implementation.md): no harness until there is a free oracle. |
| **2026-08-02** | **Round 2's five deliverables** (PRs #33–#37): **correlation coverage** (the cost instrument's own 8% blind spot, incl. the approval path); **turn-aware cost** + `token_usage_total` corrected from a lifetime sum to context occupancy; **Telegram parity** — `/model` scopes to the sticky chat, `/stop`, turn-lifecycle replies; **goal sessions in the shutdown drain**, parked durably on disk rather than left `Running`; and **Tier 3 P7 restart survival**, passing live. Every branch shipped a test claiming more than it checked — the four mechanisms and rules R6–R8 are in [round 3](future-work/parallel-deliverables-2026-08-round-3.md). |
| **2026-08-02** | **Five parallel deliverables** (PRs #28–#32), specced in [`parallel-deliverables-2026-08.md`](future-work/parallel-deliverables-2026-08.md) and executed on separate branches: **token cost accounting** (`liberado-cost` — `[[models]]` per-million rates applied at *read* time over the existing latency journal, rolled up per conversation through the dispatch journal's `parent_conversation` so delegated spend lands on the turn that caused it; the full read landed 2026-08-02 at **92.8%** orchestrator — see [P1.5](#priority-15--token-economics-foundational-measured-2026-08-02)); **per-conversation compaction trigger** (closes CH4's re-resolve gap above); **TUI stop / scoped `/model` / reattach** (durable turns had made Ctrl+S mean "stop showing me"); **graceful shutdown** (SIGTERM drains in-flight durable turns for a bounded grace before exit; `stop_grace_period: 2m` on the compose service); and **Tier 3 P6**, verified against the live daemon. Review notes and the recurring test-aim weakness are written up in [`parallel-deliverables-2026-08-round-2.md`](future-work/parallel-deliverables-2026-08-round-2.md). |
| **2026-08-01** | **Session profiles** (PR #21): per-conversation authority — a profile narrows tools, delegation, model, and prompt nudge for one conversation, and a turn may hold fewer capabilities than the profile's ceiling after per-goal narrowing. Includes **CH4's mechanism** (above), the **approval ledger** — a human's approve/reject now lives in an append-only log under `<LIBERADO_DATA_DIR>/` that no MCP mounts and no tool addresses, so editing a proposal note's `status:` authorises nothing — and the policy change that took `Write` on `proposals/` away from every agent grant. Plus the tool manifest (the model is told its exact tool surface each turn, beating stale transcript evidence), a provider-seam test sweep asserting on the serialized request body, and test-clock hardening (frozen state can no longer leak between tests, and the controls compile out of production builds). |
| **2026-07-23** | **CH3 context compaction:** long chats roll older history into a persisted summary marker (`Author::Named("compaction")` in the session DAG) — the model resumes from the summary + a verbatim tail of the last K user turns; the full transcript stays on disk, rendered and searchable. `[main_agent.compaction]` knobs, default on. Plan + 4-project research (OpenCode/Kilo/LibreChat/OpenClaw): [`context-compaction-plan.md`](future-work/context-compaction-plan.md). **Known residual** (marker durable + partial tail re-append → next load can miss unpersisted tail): documented there; preferred fix is CH3.1 viewport/side-summary — [`context-compaction-viewport-rearchitecture.md`](future-work/context-compaction-viewport-rearchitecture.md). |
| **2026-07-05** | **CH2 chat history search Tier 1** *(landed then, recorded now — the entry sat open here by mistake)*: lexical AND/regex over the session JSONL logs behind `GET /api/conversations/search`, the webui sidebar search box, and the `chat-search` MCP for the dispatcher. [`chat-search-plan.md`](future-work/chat-search-plan.md). |
| **2026-07-23** | **C1 interactive crons (AskHuman):** profile-narrowed session grants — a cron schedule naming a `[[session_profiles]]` entry whose component includes `AskHuman` gets an open input channel; unattended crons stay structurally non-interactive (D-d). |
| **2026-07-23** | **M1b hot-reload** (`apply_mcp_peer_set` / `POST /api/mcp/reload`); **lock-poisoning recovery** (sessions + catalog); **proposal expiry reaper** (configurable, background); **vault path-traversal validation** (cross-platform); **MCP test double dedup** |
| **2026-07-23** | **Architecture hardening:** god-module splits; **MCP pooling** (M1); **M1b degraded-catalog routing**; **T1** L1–L10; **A4** dual-store hub tests |
| **2026-07-19** | TurboVault plugins live (vector + tasks); Telegram dogfood baseline |
| **2026-07-18** | Cron → Telegram delivery; OpenClaw brief cutover; sticky session persistence |
| **Earlier** | Unified Session (D7); one execution engine; `Write` at MCP boundary (F1); etc. |

See [`spec/architecture/sessions.md`](spec/architecture/sessions.md) for the session model history pointers.

**Last updated:** 2026-08-02.
