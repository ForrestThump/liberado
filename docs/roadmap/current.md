# Liberado — Roadmap

Forward-looking work, beyond what `ARCHITECTURE.md` marks as built. Ordered loosely by priority
within each section. This is a living doc — promote items up as they're picked, and record *why*
something is deferred so the reasoning isn't lost.

## The phased roadmap

The matured vision (see [Positioning](../architecture/positioning.md) and
[Overview's three pillars](../architecture/overview.md)) sequences the next work into four phases.
Each phase **ships value AND advances the mesh** — the substrate falls out of feature work rather
than being built as a months-long plumbing project up front (see Decision 18, the incremental
event-bus mesh). The substrate work itself (config/policy, catalog, proposal loop — Decisions
11/14/17) is largely landed; this roadmap is what's next.

### Phase 1 — The general MCP agent (next milestone)

The vault-agnostic interactive agent; proves the core runs without TurboVault (none of it touches the
vault).

- **Route chat through the dispatcher.** Today chat drives the executor directly, bypassing the
  tool-advisor, the guards, and sub-delegation; wiring chat -> dispatcher -> orchestrator gets all
  three. (First bus-native seam: chat publishes a goal-event.)
- **Live capability catalog + on-demand tool surfacing** — the validated lazy-loading pattern; the
  token-efficiency core. (Mesh checkpoint #1: the catalog is a live, bus-queryable registry, not
  static config — the same registry the TUI/WebUI query.)
- **Multi-MCP + parallel, capability-narrowed sub-delegation** (closes Hermes gap #4).

### Phase 2 — The self-improvement moat

`ProposeMcp { spec, rationale }` -> a coding subagent builds a Rust/WASM MCP -> capability-gated
hot-reload (Hermes gap #1, containably). Reuses the Decision-11 proposal loop. (Mesh checkpoint #2:
the coding-agent is a bus service; reload re-registers in the catalog.)

**Riggers integration — the self-improvement engine, already built.** `riggers/`
(`liberado-pr-dispatch-mcp`) is a tested Rust MCP "PR factory": plain-English coding task ->
`vtcode` coding agent in a shallow-cloned repo -> draft PR -> human approval (Telegram/MCP) ->
publish. Never auto-merges. This collapses most of Phase 2 (and also accelerates Phase 1 — it is the
first real MCP to dogfood the dispatcher and catalog against). Integration plan:

1. **Use it as an MCP, do not absorb its code.** Register it in `topology.mcps` (e.g. `code-dispatch`,
   `consequence = external`); the dispatcher routes build/fix tasks to it and the consequence guard +
   proposal loop gate it. This is maximally aligned with the MCP-first / loose-coupling pillars — a
   standalone capability slotting in with near-zero coupling.
2. **Switch riggers from its direct OpenRouter HTTP client to the shared `Provider` trait**
   (`liberado-provider`), making it provider-agnostic (any OpenAI-compatible endpoint/model).
   Rationale: `vtcode` fans out subagents, so a single-provider pattern rate-limits fast
   (DeepSeek-on-DeepSeek); routing through the trait lets riggers use OpenRouter or another model
   *through the shared abstraction*, and centralizes provider logic. A small, deliberate trade of
   coupling for versatility.
3. **`ProposeMcp` specialization** — a build-task variant targeting a new Liberado MCP crate plus the
   hot-reload/registration step, so the agent can extend its own toolset, capability-gated. This is
   the Phase-2 moat, now mostly a thin layer over riggers.
4. **Dogfooding migration** — riggers currently feeds OpenClaw (`OPENCLAW_WEBHOOK_URL`); pointing it
   at Liberado is the first "move one workflow over" migration.
5. **Its draft-PR -> approve/revise/reject lifecycle parallels the Decision-11 proposal loop** (a PR
   is the ideal proposal artifact for code). The two approval surfaces specialize cleanly: vault
   `proposals/` for general high-consequence actions, draft-PR for code.

**Patterns to lift into the core later (not absorb-whole):** the token-budgeted `explorer` (serves
the token-efficiency pillar and a future ContextPolicy), the `refiner` (~= Clarify), `forge-client`,
and the GIT_ASKPASS credential hygiene.

### Phase 3 — Autonomy breadth

Cron as a bus listener (Hermes gap #2, near-free) + the vault becomes the reactive event-source
plugin (vault-decoupling lands here, behind an event-source/ACP trait). (Mesh checkpoint #3: cron and
vault-watch are interchangeable event-sources; a second dispatcher/executor is config-enableable.)

### Phase 4 — Scaling

An `ExecutionEnvironment` trait (Local / Docker / serverless hibernation) — Hermes gap #3; cheap
always-on.

## Landed (substrate already built)

- **Deterministic consequence guard (§6 #3)** — ✅ *done (per-MCP reversibility/externality)*. Gates
  direct action by `Consequence` (read-only < reversible < irreversible < external). Validated in
  `liberado-eval`: external actions (email/Slack) are deterministically downgraded to `Clarify` even
  at high confidence, while git-tracked vault writes flow.
- **Magnitude / destructiveness axis** — ✅ *dispatcher-level done.* A liberado-owned, deterministic
  classifier (`Magnitude`, `is_sweeping_destructive` in `common` — reads the goal/tool text, needs no
  MCP metadata) gates **sweeping-destructive** goals even when reversible. Closed the eval's UNSAFE=1:
  "delete all my notes" → `Clarify` (0.90, guard downgrade) while "delete tmp.md" still flows.
  - **Remaining: per-call runtime enforcement.** The dispatcher only sees the *goal* (the signal that
    survives the model routing to a subagent); the fuller layer classifies the **actual tool call +
    args** at the runtime (a `RiskGatedToolRuntime` wrapping the executor's `ToolRuntime`), where a
    subagent's concrete `vault:delete` with a wildcard arg is visible. Same liberado-owned classifier,
    enforced in-band (refuse → the model must report/confirm). This is where per-tool *args-aware*
    magnitude lives.
- **Proposal workflow (Decision 11)** — ✅ *done (emit AND approve→execute landed, June 24, 2026).*
  The full propose→approve→execute loop is closed: a human `status: approved` edit on a
  `proposals/<id>.md` note is picked up by the daemon, executed via the orchestrator with the
  proposal's `correlation_id`, and flipped to `status: done`. **Remaining:** broaden emit beyond the
  concrete-tool-call case (empty-seed `ExecuteDirect`, `DispatchSubagent`, the magnitude gate),
  which still downgrade to `Clarify`.
- **Zone write-class guard (§6 #2)** — downgrade agent writes to `proposal_only` / `human_only`
  zones to a Proposal (Decision 11), using the existing `WriteClass`.
- **Catalog population** — the live daemon dispatches against an *empty* MCP catalog today; build the
  catalog (names + descriptions + consequence) from the registered `McpRegistry` servers so the
  dispatcher can actually route in production. (Phase 1 graduates this into a live bus-queryable
  registry.)
- **Runtime tool gating** — enforce capability/consequence on the executor's *adaptive* calls (not
  just the classifier's pre-flight opening move), since an `ExecuteDirect` with empty `seed_calls`
  can call anything in scope.

## Nice to haves

### Independent safety rater (the "second opinion" model)

A separate, **cheap, completion-unbiased** model that rates an incoming goal for **danger** (and
optionally ambiguity), independent of the dispatcher/executor. Motivation: a model in a task-oriented
role carries a "get it done" pull that can subtly under-rate danger; an independent judge with no
stake in completing the task rates more conservatively.

Design constraints if/when built:
- **Downgrade-only** — like the deterministic guards, it can force `Clarify`/propose, never
  *authorize* action. It joins the "can only reduce autonomy" layer.
- **Actually independent** — ideally a *different model family* than the executor (a DeepSeek rating
  a DeepSeek shares failure modes; the value is in *decorrelated* judgments). We're provider-agnostic
  (Decision 13), so a different vendor for the "safety" role is a config change.
- **Danger > ambiguity** — danger is separable from routing and worth an independent signal;
  ambiguity requires understanding the goal to route it *anyway*, so a separate ambiguity rater is
  mostly redundant with the dispatcher.
- **Best placement** — a cheap **pre-dispatch triage**: rate danger first, short-circuit to
  `Clarify`/propose on a high score *before* paying for the dispatcher/executor. Improves safety and
  saves cost.

**Why it's deferred (not skipped):** the deterministic guards are strictly better for *enumerable*
danger (can't be talked out of it, free, exact) — that's the first line and the architecture's thesis.
The rater earns its place only on the *non-enumerable long tail*, and only if it **measurably** lifts
the safe-default / safety-regression metrics in `liberado-eval` (A/B "dispatcher alone" vs "rater +
dispatcher" on adversarial danger scenarios). Prove it on the instrument before adding it to the hot
path; don't add it on intuition.

### Other

- **MCP connection pooling / reuse** — today a fresh connection per execution. TurboMCP's
  `SessionManager` (single-transport-type) is worth recycling per transport group for pooling +
  health + reconnection.
- **Multi-server MCP registry UX** — declare several servers (stdio `npx` + remote HTTP like
  deepwiki) from config; the machinery (`McpRegistry`, mixed `McpConnector`s) exists, the
  config/registration surface does not.
