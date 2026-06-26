# Liberado — Roadmap

Forward-looking work, beyond what `ARCHITECTURE.md` marks as built. Ordered loosely by priority
within each section. This is a living doc — promote items up as they're picked, and record *why*
something is deferred so the reasoning isn't lost.

## Next

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
  dispatcher can actually route in production.
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
