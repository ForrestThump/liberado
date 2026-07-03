# Liberado — Roadmap

Forward-looking work, beyond what [`docs/architecture/overview.md`](../architecture/overview.md)
marks as built. Ordered loosely by priority within each section. This is a living doc — promote
items up as they're picked, and record *why* something is deferred so the reasoning isn't lost.

## The phased roadmap

The matured vision (see [Positioning](../architecture/positioning.md) and
[Overview's three pillars](../architecture/overview.md)) sequences the next work into four phases.
Each phase **ships value AND advances the mesh** — the substrate falls out of feature work rather
than being built as a months-long plumbing project up front (see Decision 18, the incremental
event-bus mesh). The substrate work itself (config/policy, catalog, proposal loop — Decisions
11/14/17) is largely landed; this roadmap is what's next.

### Phase 1 — The general MCP agent ✅ (done, 2026-07-02)

The vault-agnostic interactive agent; proves the core runs without TurboVault (none of it touches the
vault).

- ✅ **Route chat through the dispatcher.** Chat now runs every turn through `Dispatcher::dispatch`
  before executing: `ExecuteDirect` falls through to the existing streaming path (zero UX
  regression, verified live over SSE), `Clarify`/`Propose`/`DispatchSubagent` route through the
  orchestrator. Full writeup:
  [chat-dispatcher-and-component-scoping.md](chat-dispatcher-and-component-scoping.md).
- ✅ **Live capability catalog + on-demand tool surfacing.** The three independently-static catalog
  copies (daemon, chat, API) are now one shared `Arc<CapabilityCatalog>`, snapshotted fresh per
  dispatch instead of frozen at boot. The dispatcher's `ExecuteDirect` decision now narrows which
  MCPs' tool schemas the executor surfaces (`DispatchAction::ExecuteDirect.relevant_mcps`,
  configurable via `DispatchTuning::narrow_direct_tools`, default on) — verified live: a PDF goal
  and a memory-search goal each correctly narrowed to just the one relevant MCP out of a two-MCP
  grant. Full writeup:
  [live-catalog-and-dispatcher-narrowed-tools.md](live-catalog-and-dispatcher-narrowed-tools.md).
- ✅ **Multi-MCP + parallel, capability-narrowed sub-delegation** (component-scoping half).
  `Grant.component` is now consulted (`"main-agent"` vs `"dispatcher"`), and the `ExecuteDirect`
  runtime-scoping gap that made this toothless is closed — see the dispatcher-and-component-scoping
  writeup. Parallel sub-delegation itself (`Orchestrator::dispatch_parallel`) already existed;
  wiring more of the fleet through it live is still open (closes Hermes gap #4 fully once the
  deferred MCPs — caldav, calorie-counter, weather pending an upstream stdio fix — are usable too).

### Phase 2 — The self-improvement moat ✅ (done, June 2026)

Self-extension is, at bottom, **the agent using a code-building MCP** — no new dispatch-action,
proposal, or capability type. Register riggers as `code-dispatch` (`consequence = reversible`, so a
run only ever produces a draft PR), add a *greenfield* mode so it can scaffold a brand-new MCP from
scratch (not just modify an existing repo), and the **draft-PR -> human review -> merge is the single
gate**. A new tool is an external MCP in its own repo, wired in by a human `topology.toml` edit
(Decision 14 — the daemon never writes config) + a restart. Hermes gap #1, containably. Full plan:
[phase-2-implementation-plan.md](phase-2-implementation-plan.md). Implementation report:
[phase-2-implementation-report.md](phase-2-implementation-report.md). MCP hot-reload and the EventBus
("mesh checkpoint #2") are **deferred** — riskiest, lowest value now, and runtime config-writes would
break Decision 14; a restart activates the merged MCP.

**Riggers — the self-improvement engine, complete.** `riggers/` (`liberado-pr-dispatch-mcp`) is
a tested Rust MCP "PR factory": plain-English coding task -> `vtcode` coding agent in a sandbox ->
draft PR -> human approval (Telegram/MCP) -> publish. Never auto-merges. The three slices are all
shipped:

1. ✅ **Register it as `code-dispatch` (`reversible`), grant `ExecuteMcp("code-dispatch")`, and switch it
   to the shared `Provider` trait** (`liberado-provider`). Delivers *modify-existing* self-improvement
   immediately, at near-zero coupling (MCP-first pillar).
2. ✅ **Add a greenfield mode** — the one genuinely new capability. Greenfield scaffolds a fresh MCP
   project (`cargo new`/template), runs the `vtcode` loop until `cargo test` passes, and opens a
   draft PR on a *new* repo (the ForrestThump fork). Plus catalog triage: tool exists -> `modify`;
   absent -> `create`.
3. ✅ **Eval + docs + dogfooding migration** — 6 eval scenarios verify the single-gate and
   capability-non-widening guarantees. Human wire-in workflow documented. Tuning config
   (`max_concurrent_coding_subagents`) added. `McpDescriptor.provenance` field added for
   self-extension traceability.

**Patterns to lift into the core later (not absorb-whole):** the token-budgeted `explorer` (serves
the token-efficiency pillar and a future ContextPolicy), the `refiner` (~= Clarify), `forge-client`,
and the GIT_ASKPASS credential hygiene.

### Phase 3 — Autonomy breadth

Cron as a bus listener (Hermes gap #2, near-free) + the vault becomes the reactive event-source
plugin (vault-decoupling lands here, behind an event-source/hook trait). (Mesh checkpoint #3: cron and
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
  - ✅ **Per-call runtime enforcement — done (2026-07-02).** `Orchestrator`'s `ExecuteDirect` /
    `DispatchSubagent` / `dispatch_parallel` paths now wrap their runtime in `RiskGatedToolRuntime`
    (relocated to `liberado-executor` to avoid a circular crate dependency with `liberado-mcp`), so
    the executor's *adaptive* (non-seed) tool calls get the same capability/consequence/magnitude
    checking the dispatcher's pre-flight guard only ever applied to the seed call.
    `execute_approved` is deliberately left ungated (approval is already the authorization). 5 new
    integration tests in `crates/orchestrator/tests/orchestrate.rs` prove the gap is closed.
  - ✅ **Runtime-gated proposals now land in the vault — done (2026-07-02).** That runtime gate's
    downgraded proposals used to write to a data-dir path nothing ever read (a dead end — see
    [hardening-audit-2026-07-02.md](hardening-audit-2026-07-02.md) item 3). `RiskGatedToolRuntime`'s
    `proposals_dir` now means the vault's own `proposals/` directory, so a runtime-gated downgrade
    flows through the exact same approve→execute pipeline pre-flight proposals already use — proven
    end-to-end by `daemon`'s
    `runtime_gated_downgrade_lands_in_the_vault_and_executes_once_approved` test.
  - ✅ **Proposal integrity signing — done (2026-07-02).** Every `Proposal` now carries an
    HMAC-SHA256 `integrity` signature (`ProposalSigner`, per-installation key) over its immutable
    fields, verified before execution in `handle_proposal_change` and again (defense-in-depth) in
    `execute_approved`. Closes hardening-audit item 2 (action substitution/tampering detection).
    Item 1 (writer-identity verification — *who* flipped `status: approved`) stays open; it needs
    OS-level MCP process isolation or an out-of-band approval channel, not a code patch — see that
    audit doc's item 1 for why.
- **Proposal workflow (Decision 11)** — ✅ *done (emit AND approve→execute landed, June 24, 2026;
  integrity signing + vault-routed runtime proposals added 2026-07-02; `DispatchSubagent` emit
  broadened 2026-07-02).*
  The full propose→approve→execute loop is closed: a human `status: approved` edit on a
  `proposals/<id>.md` note is picked up by the daemon, verified for integrity, executed via the
  orchestrator with the proposal's `correlation_id`, and flipped to `status: done`. A high-consequence
  `DispatchSubagent` now downgrades to `Propose(ProposedAction::Subagent)` instead of `Clarify` —
  it always carries a restated goal, so there's always something concrete to propose. On approval,
  `Orchestrator::execute_approved`'s new `Subagent` arm dispatches it through the same runtime-gated
  execution a live `DispatchSubagent` gets (unlike the `ToolCalls` arm, which is ungated — what was
  approved there was the exact calls, not just a goal + scope). **Remaining:** the one still-deferred
  fuzzy case is an empty-seed `ExecuteDirect` (including a bare magnitude-gate hit with no seed
  calls) — no fixed action to propose there since `ExecuteDirect` carries no goal of its own; see
  `downgrade`'s doc comment in `crates/dispatcher/src/lib.rs` for the shape a follow-up would take.
- **Crate hygiene + hardening passes (2026-07-01 to 2026-07-02)** — three hygiene tiers (test-mock
  dedup, `RuntimeFactory` relocation to `liberado-executor`, new `liberado-config` crate extracted
  from `liberado-bootstrap`) plus a hardening audit (proposal-integrity items above). Full writeups:
  [hygiene-audit-2026-07-02.md](hygiene-audit-2026-07-02.md),
  [hardening-audit-2026-07-02.md](hardening-audit-2026-07-02.md),
  [crate-modularity-audit.md](crate-modularity-audit.md) (a broader coupling/duplication sweep;
  items 1, 2, 4, 5 done, item 3 — splitting `liberado-common` — still deferred).
- **Zone write-class guard (§6 #2)** — downgrade agent writes to `proposal_only` / `human_only`
  zones to a Proposal (Decision 11), using the existing `WriteClass`.
- **Catalog population** — the live daemon dispatches against an *empty* MCP catalog today; build the
  catalog (names + descriptions + consequence) from the registered `McpRegistry` servers so the
  dispatcher can actually route in production. (Phase 1 graduates this into a live bus-queryable
  registry.)
- ✅ **Runtime tool gating** — done, see "Per-call runtime enforcement" above (same item, noted twice
  in this doc).
- **Shared wire-type + slash-command extraction across clients** — TUI, WebUI, and CLI now share
  `chat-client-contract`'s `ChatEvent`/SSE decoder and `liberado-commands`' slash-command dispatcher
  instead of three hand-rolled copies. Full plan:
  [tui-shared-code-extraction-plan.md](tui-shared-code-extraction-plan.md) (decoder unification
  done; the plan's `ChatClient` trait adoption is a separate, still-deferred follow-up — see
  `crate-modularity-audit.md` finding 2).
- **WebUI flesh-out** — sidebar, MCP panel, markdown rendering, slash commands, and chat UX landed.
  Design reference, not a live TODO list: [webui-flesh-out-plan.md](webui-flesh-out-plan.md).

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
- **A2A (Agent2Agent) interop** — captured as an idea, not scheduled:
  [`a2a-protocol-idea.md`](../ideas/a2a-protocol-idea.md). The conversation-store seams
  (`author`, conversation lineage — Decision 17) and the mesh direction (Decision 18) already
  carry most of what this needs; the real gap is a new inbound protocol surface (AgentCard +
  Task lifecycle) and an outbound peer-delegation capability. Not before Phase 3 — same category
  of work as vault-decoupling and cron (another event-source in, another external capability
  out).
