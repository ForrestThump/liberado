# Liberado — Roadmap

**What is not done yet.** Future-looking work only. What *is* built is described in [`spec/architecture/overview.md`](spec/architecture/overview.md); finished plans and closed audits are in [`future-work/archive/`](future-work/archive/README.md). Future work index: [`future-work/README.md`](future-work/README.md).

Before starting anything, read [`spec/architecture/failure-modes.md`](spec/architecture/failure-modes.md) — six bug classes this codebase produces over and over. Operator knobs (and which are compiled in) are in [`spec/reference/tuning.md`](spec/reference/tuning.md). Every one of them shipped with a green test suite.

> **Picking up self-scoped work?** Start at [`future-work/backlog.md`](future-work/backlog.md) —
> one item per PR, verify it is still open before starting, and paste per-changed-behaviour mutation
> evidence in the PR body. This page is the *why*; the backlog is the *what next*.

## Open now — in priority order

> **Active focus (2026-08-11):** the autonomous PR machine. See
> [The autonomous PR machine — fastest path](#the-autonomous-pr-machine--fastest-path-set-2026-08-11)
> for the ordered list; it cuts across P1–P3 and takes precedence while it runs.

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

### Priority 3 — coding pack (best accepted result per dollar)

> **Pulled forward by owner decision (2026-07-24):** the agentic coding TUI track is now active
> alongside P1/P2 — plan: [`coding-tui-plan.md`](future-work/coding-tui-plan.md) (goal-driven TUI surface +
> kernel completion gate, Grok Build-style disputed-claim completion, slices S1–S7).

**Performance target (set 2026-08-11):** best-in-class accepted-result-per-dollar on DeepSeek v4
Flash for scoped PR work, while preserving the general kernel, capability model and domain-pack
boundary. “Comparable” means a merge-ready result under the same task, repository commit, model,
provider and resource caps — not matching another harness's TUI or copying its framework.

**Self-hosting status (2026-08-09).** The bar in the backlog is "run these PRs on our own coder
instead of OpenCode". It is now cleared once: [PR #90](https://github.com/ForrestThump/liberado/pull/90)
was written end-to-end by the coding pack running unattended, compiles, passes CI on both platforms,
and includes tests the agent wrote — with two compile errors it found and fixed itself by running
cargo and reading the output.

Getting there took five harness fixes ([PR #91](https://github.com/ForrestThump/liberado/pull/91)),
all of the same shape: **the guards were right that something was wrong and wrong about the remedy.**
A cycle detector compared tool names and ignored arguments, so normal exploration read as a loop. A
latched progress guard refused the very edits it was demanding, because the escape hatch written for
exactly that case sat behind an early return and had never executed. A doom-loop strike ended the
run rather than refusing the call, discarding ten files of correct work two turns before the build
step that would have caught its one mistake. None of these were model failures.

**Runs are now replayable.** Every coding run writes a trace (`[coder] trace_dir`,
`[coder] trace_formats`) recording what was sent, what the model said verbatim, which tools it was
*offered* each turn, and why each turn ended. Since PR #117 it also records **what the model was
sent** — tools offered at request time, and the system prompt itself, once per distinct hash. Before
this, none of that existed anywhere, and four consecutive failures were each diagnosed by reading
Rust and guessing. **Start a review of an agent run by reading its trace.** An `openai-messages`
export is also available for comparing a run against another harness on the same task and model.

**Reliability push, 2026-08-09/10 — read this before touching the pack.**
[`coder-harness-reliability-2026-08.md`](future-work/coder-harness-reliability-2026-08.md) is the
current record: an A/B against Kilo Code on the same model and task, the fourteen defects it exposed
(PRs #106–#119), and — most valuable — **three plausible hypotheses that were tried and did not
work**. Edit failure fell from ~66% to 8%. The pack still has not been shown to complete an
unattended task end to end, and the 8% figure comes from a different task than the baseline, so it
is a signal rather than a controlled result. Do not repeat "the model is not good enough": the same
model in another harness did the work.

**Slice status** (plan: [`coding-tui-plan.md`](future-work/coding-tui-plan.md) §Slices):

| Slice | State | Notes |
|---|---|---|
| **S1** — completion gate | ✅ **landed** | `liberado_session::completion_gate` (gatekeeper veto + strict-majority fresh quorum + fail-closed votes), coding-pack adapter, `critic_verdict` on the wire, strategist on non-convergence. **Default OFF** (`[coder.gate] enabled`) — it costs `1 + fresh_reviewers` model calls per attempt, and stays opt-in until S7 measures it. |
| **S2** — wire events + goal surface | 🟡 **partial** | Done: `file_changed`, first-class `hub.park()` + `POST /api/goals/{id}/park`, `/goal` commands (`start`/`in`/`status`/`pause`/`resume`/`clear`), TUI wiring for all of them; **live dogfood run 2026-08-05** (self-host → [PR #69](https://github.com/ForrestThump/liberado/pull/69); write-up [`self-host-coding-dogfood-2026-08.md`](future-work/self-host-coding-dogfood-2026-08.md)). **Not done:** dedicated goal-view panes; tool/`file_changed` events still weak on the live session stream (see dogfood finding #4). |
| **S3–S7** | 🟡 **partial** | **S3** project auth landed (PR #66). **S4** checkpoints + mid-build resume + rewind landed (PR #73). **S5** `/loop` still unbuilt (largest design-ready zero-code gap). **S6** fan-out + merge-back landed (PR #72). **S7** strategist evals + gate default-on still open. Still open: ship package / cold-review productization, plan approval UX |

Two carried-forward limitations worth knowing before building on this:

- **Gate votes reach the wire batched at attempt end, not live per vote.** The kernel's
  `GateObserver` supports live emission; `CoderBackend::run` has no `SessionEvent` sender to plumb
  it through. Wiring one is the remaining half of "watch the quorum vote".
- **Compaction tail copies still exist on disk** (CH3.1 territory) — any *new* reader that walks a
  raw leaf path must skip `Author::is_compaction_tail_copy()`.

| # | What | Why |
|---|---|---|
| **CT1** | **Agentic coding TUI** — [`coding-tui-plan.md`](future-work/coding-tui-plan.md) | `/goal` + critic-gated completion + `/loop`, on the existing TUI/hub/coding pack; loosely coupled kernel machinery. S1 done, S2 partial (see above) |
| **SP1** | **Self-PR quality loop** — [`self-pr-quality-roadmap.md`](future-work/self-pr-quality-roadmap.md) | Ship package → cold review → fix → human residual review. Path to light-oversight self-PRs on liberado |
| **E6-c(b)** | ~~Resume mid-build coding session~~ | **Landed** as S4 (PR #73: durable worktree + shadow-git checkpoints + park/resume + rewind) |
| — | [`self-host-coding-dogfood-2026-08.md`](future-work/self-host-coding-dogfood-2026-08.md) | **C2 dogfood findings** — reliability fixes partially landed; continue as grading method for SP1 |
| — | [`pr-dispatch-vtcode-no-write-finding.md`](future-work/pr-dispatch-vtcode-no-write-finding.md) | Open bug |
| — | [`coder-eval-curriculum.md`](future-work/coder-eval-curriculum.md) | After P1/P2 not bottleneck |

### Cross-cutting

- **Model View Log (MVL)** — [`spec/reference/model-view-log.md`](spec/reference/model-view-log.md).
  A cross-harness contract for *what the model actually saw and did*: JSONL, flushed per event,
  with a reconstruction requirement (rebuild the exact messages **and tool catalogue** for any turn
  from the log alone). It is paired, not conflated, with an execution log for tool timing,
  concurrency, attempts, gates and worker-graph edges. Written so Liberado, Kilo Code, pi, Hermes
  and Deep Agents can all be scored by one parser instead of a translation layer per harness. Our
  `CoderEvent` stream is close; the gaps are listed at the end of the spec.
- **What to take from pi, Hermes and Deep Agents** (research, 2026-08-11) —
  [`harness-study-2026-08.md`](future-work/harness-study-2026-08.md). Their cost advantage is
  structural, not prompt wording: they keep tokens **out** of the window (offload oversized tool
  results to disk; batch many tool calls into one scripted turn) rather than shortening them.
  Ranked by leverage for us, with an explicit "do not copy" list. All three are MIT.
- **Per-model knob profiles and a tuning ledger** (design, 2026-08-11) —
  [`model-knob-profiles.md`](future-work/model-knob-profiles.md). The harness–model *pairing*
  matters more than either alone, and we already have per-model findings hardcoded as shared
  constants. Profiles applied automatically per model, `extends` so a new release starts from its
  nearest relative, and a SQLite ledger recording resolved knob values with each run's outcome.
  **Deliberately not scheduled** — the prerequisites are listed, and the first is that
  `config_literal_rules.rs` currently guards exactly one config type.
- **Cadence-triggered maintenance agents** (idea, 2026-08-10) — dispatch an agent automatically
  after N commits or N merged PRs, bound to a named skill (doc updates, test-coverage sweeps,
  mutation runs, architecture critique), so periodic work reaches the PR pipeline without anyone
  remembering to ask. **Most of the spine already exists** — `EventSource`, the `Event` envelope,
  `GoalSessionHub::start_background`, `DomainHint::Coding`, `Skills/`, the fan-out cap — so the gap
  is a `RepoEventSource`, a trigger→skill binding, and durable counter state. Audit, the
  self-triggering hazard, and the reason **not to build it yet**:
  [`cadence-triggered-maintenance-agents.md`](future-work/cadence-triggered-maintenance-agents.md).

- **External dependency audit** — audit all `Cargo.toml` entries across crates for unnecessary duplication, unused deps, version drift, and opportunities to share/slim. Goal: reduce compile wall-clock without breaking anything.
- **Modularity** remains the enabler: [`spec/architecture/modularity.md`](spec/architecture/modularity.md). Hot-path **module splits** landed (server API, daemon, config-loader model, executor budget).
- **A4 dual-store hub tests** (2026-07-23): list / cancel / park→resume / rehydrate via real `GoalSessionHub` on production `SessionStore` — `crates/session-store/tests/hub_dual_store.rs` (see [`spec/architecture/failure-modes.md`](spec/architecture/failure-modes.md) §1).
- **TurboVault modules**: vector + tasks paying back; remaining **`vault_events`** and upstream merge. Umbrella: [`turbovault-modules-integration-roadmap.md`](future-work/turbovault-modules-integration-roadmap.md).
- **Remote access via Paseo**: fork [Paseo](https://github.com/getpaseo/paseo) and mate Paseo + Liberado so the Liberado harness (daemon, TUI, API, coding sessions) can be accessed remotely. Paseo provides secure tunnel/remote-access primitives; Liberado's daemon + HTTP/SSE surface is naturally compatible. Ordered work is **Phase 6 (Track B)** in [`future-work/paseo-liberado-integration-roadmap.md`](future-work/paseo-liberado-integration-roadmap.md) — keep separate from the ACP coding agent.
- **Paseo coding agent (ACP) — landed for local use (2026-08-09):** `liberado-acp` is a real ACP agent (`session/new` / `session/prompt` / `session/update`) with coding tools; register via Paseo `extends: "acp"`. Install: `scripts/install-paseo-liberado.ps1` · docs: [`impl/paseo-integration.md`](impl/paseo-integration.md). **Ordered residual backlog:** [`future-work/paseo-liberado-integration-roadmap.md`](future-work/paseo-liberado-integration-roadmap.md) (P0: tool-call ids, resume honesty, `--version`; then tests, modes, durable load, fork polish). Older pointer: [`future-work/archive/acp-bridge-completion-roadmap.md`](future-work/archive/acp-bridge-completion-roadmap.md) (superseded).
- **Redundant tool calls hidden by the doom-loop guard** (found 2026-07-28 in the passing
  `evening-debrief` live run, build `66b5771`). The subagent called `liberado-caldav-mcp:list_events`
  **four times for two dates** — twice on turn 2, twice again on turn 3 — before the guard fired
  (`doom loop detected; nudging once`), after which it recovered and filed on turn 4. The run
  **succeeded**, which is the point: the guard is currently absorbing a 2× redundancy rather than the
  redundancy being fixed, so it shows up as latency and spend, not as a failure. Worth a look when
  the executor's tool loop is next touched — the guard should stay, but it should be catching
  pathology, not routine duplication.
- **Move-on bar:** leave P1 when you daily-drive without wincing — not when polished.

## The autonomous PR machine — fastest path (set 2026-08-11)

**Goal:** dispatch a scoped task and get back a PR whose review is taste and scope, not repair.
This table is the harness track. The repo-wide total order is the
[backlog implementation order](future-work/backlog.md#implementation-order); F9, 0.1b, D2
(#154), 0.6, B1, C1, C7 (#166), 0.9 (#167) and the progress-guard have landed. The next
harness *code* is **0.10** (ship-bar excerpt: name the failing crate, not the last
crate). **0.7 / C3** is still the next *report*, not the next fix. Vary the task;
close the last failure class before the next live compare. The evidence says
something specific: **every measured improvement so far came from fixing a defect, not from tuning
a value.** Edit failure went 66–70% → 8% → 0% across PRs #106–#128, all defect fixes. No knob has
yet been tuned to a measured gain.

| # | Do | Why it is here |
|---|---|---|
| ~~**1**~~ | ~~**Wire the ship preflight into the ACP dispatch path**~~ **Landed, PR #134.** | Built in PR #74 and `coding_run.rs` never called it, so every dogfood run to date skipped the ship bar. The wiring was the small half: the bridge also loaded **no config at all** — it read `LIBERADO_CONFIG_DIR` directly rather than through `liberado_config::config_dir()`, and nothing set the variable, so there was no declared project to have a bar. It now logs the config dir it resolved. |
| ~~**2**~~ | ~~**Close the two open harness bugs**~~ **Landed, PRs #131 / #132.** | `validate` passing read as "done" (a run shipped seven failing tests as success), and an empty critic response discarding a completed run. [`coder-harness-reliability-2026-08.md`](future-work/coder-harness-reliability-2026-08.md) |
| ~~**3**~~ | ~~**Make one production coding-run assembly path.**~~ **Landed, PR #141.** | `assemble_production_run` now serves `CodingSessionPack`, ACP and the headless runner. Mechanical rules bind each real entry point to it, while preserving surface-owned trace provenance. |
| ~~**4a**~~ | ~~**Finish the MVL and execution-log contracts.**~~ **Landed, PR #140.** | Exact request metadata, unambiguous call-ID joins, context-reset snapshots, attempt pairing and shared conformance fixtures are now the stable waist. |
| ~~**4b**~~ | ~~**Emit the joined logs from the common boundary.**~~ **Landed, PR #151.** Implement append-and-flush at the `executor` / provider boundary, not only in `coder-agent`. | The instrument. Every later claim — knob, prompt, cache, graph or harness — is unmeasurable without it. |
| **5** | **Run a controlled cross-harness baseline.** Add minimal emitters to the pinned pi, Hermes and Deep Agents forks; then run Liberado and the references on the same user task, repository commit, model, provider, sampling settings and resource caps. Keep each harness's native system prompt and tool schemas. | Source reading gives hypotheses. Repeated A/B/C/D runs say which mechanisms improve the accepted result, cost and latency. This comes before copying another architecture. |
| ~~**6**~~ | ~~**Productize cold review + one fix round.**~~ **Landed, PR #142.** | Fresh review is bound to changed paths and excerpts; one retained-finding fix round must be reverified before readiness. |
| ~~**7**~~ | ~~**Implement the first evidence-selected cost lever.**~~ **Landed, PR #167.** Oversized tool results spill to `.liberado/offload`; the tail stays reachable. Cache policy, context selection and structured compaction still follow only where a baseline supports them. | Change one mechanism at a time. The previous benchmark changed several settings together and cannot attribute the gain. |
| **8** | **Extract knobs where a measurement says the constant is wrong.** | Not "as many knobs as plausible". See the note below. |
| **9** | **Per-model profiles, then the SQL tuning ledger.** | Correct to defer while we run only DeepSeek v4 pro/flash. [`model-knob-profiles.md`](future-work/model-knob-profiles.md) |

**On knobs, a caution.** A knob is only an asset once something has been tuned with it; until then it
is surface area. Ten settings have shipped that parsed, validated and reached nobody, and
`config_literal_rules.rs` currently guards exactly one config type. Adding knobs ahead of #4b–#5
means adding untested configuration to a harness whose defects are still being found, and measuring
the result with an instrument that does not exist yet. The cheap discipline: when a measurement
shows a constant is wrong — as #128 showed for per-tool argument matching — extract *that* constant
and add it to the mechanical guard in the same PR.

**On taste.** The thing that makes a PR match a particular person's preferences is the review layer
and the prompts in `prompts/`, not the knobs. The taste lever landed in #142; #7–#8 remain
performance levers.

### How this track is scored

The primary measure is **accepted result under a fixed budget**, not tool-call style. Report:

1. task/ship-gate pass rate and merge-ready rate;
2. total cost per accepted result, including retries and reviewers;
3. wall-clock p50/p95 and turns used;
4. human repair time or repair diff after the run; and
5. classified failure mode with trace pointers.

Reads per successful edit, tool counts and tools withdrawn explain a result; they do not rank
harnesses. Run one-variable mechanism experiments. The first 10-task dogfood changed turns,
attempts, verifiers, hashline, prompt and context together, so it established a useful combined
configuration and did **not** establish which one caused the gain. Use repeated runs; one sample is
not a model ceiling.

### Graph, goal, loop and surface order

- **`/goal` quality first:** `Succeeded` stays verifier/ship-backed. Cold review + one fix round,
  restart reconciliation, non-interactive honesty and signal-time work preservation landed in PRs
  #142–#144 and #138. Stage ship-preflight output next; keep headless terminal exit codes honest.
- **Prove S6 before making it smart:** live-dogfood fan-out through the real build path, including a
  parent verifier after merge, conflict handling and partial-child failure. Then accept a typed work
  graph proposed by a model and validated/scheduled by code. Nodes declare dependencies, effects,
  isolation, outputs, verifiers and budgets. No general swarm or nested fan-out yet.
- **`/loop` remains a product lane, not a coding-performance lever:** it is a durable scheduler over
  ordinary goals. Build it after goal completion is repeatably trustworthy.
- **Surfaces remain clients:** track kernel economics/reliability, coding-pack performance and chat
  UX on three separate scoreboards. A LibreChat-quality WebUI must not own agent control flow.

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

  P3 autonomous PR ──► F9 landed (#146)                (safety)
                    ├── 0.1b landed (#147)             (staged preflight)
                    ├── D2 landed (#154)               (cost prerequisite)
                    ├── 0.6 landed (#151)              (instrument)
                    ├── B1 landed (#162)               (ExecuteDirect delivery)
                    ├── same-session check (#163)      (refuse red succeeded)
                    ├── C1 landed (#164)               (deny shell git; gix tools)
                    ├── progress-guard (#165)          (report beats churn fatal)
                    ├── C7 landed (#166)               (isolated parallel door)
                    ├── 0.9 landed (#167)              (offload oversized results)
                    ├── 0.10 ship-bar excerpt          (NEXT: failing crate, not last-N)
                    ├── 0.7/C3 controlled baseline     (measurement)
                    └── C5 completion-gate comparison  (decision)

  Full total order ──► docs/future-work/backlog.md#implementation-order

  Later ──► W1 mobile WebUI session view
  TurboVault (parallel) ──► vault_events · upstream land
```

## Recently landed

| When | What |
|------|------|
| **2026-08-14** | **0.9 offload.** Oversized command output and (when a spill directory is set) any oversized tool result are written under `.liberado/offload`; the model sees a head+tail preview and can `read_file` the rest ([#167](https://github.com/ForrestThump/liberado/pull/167)). Under the threshold, or with no directory, the result is unchanged. Backend gates stay truncated. |
| **2026-08-14** | **C7 + host-failure.** `dispatch_parallel` is reachable through the dispatch pack: optional `workspace_root` plus `RuntimeFactory::runtime_for_in`, with `WorktreeWorkspace` per worker ([#166](https://github.com/ForrestThump/liberado/pull/166)). `delegate` stays synchronous. A host infrastructure failure (disk full, and the same class) ends the run; the executor files `Failed` and does not give the model another turn. |
| **2026-08-14** | **C1 + progress-guard.** Default `CommandPolicy` denies shell `git` (and `git.exe` by stem); dedicated `git_*` tools go through `coder-tools::git` ([#164](https://github.com/ForrestThump/liberado/pull/164)). A filed report is no longer rewritten to `NoChanges` by a progress fatal; `run_command` is not same-tool-churn; mutating cycles match on exact args ([#165](https://github.com/ForrestThump/liberado/pull/165)). |
| **2026-08-13** | **B1 + same-session compile.** `ExecuteDirect` now carries `Delivery` ([#162](https://github.com/ForrestThump/liberado/pull/162)): a research chat relay gets the relay contract, acting work stays short, vault delivery files the report. `submit_report outcome=succeeded` is refused while `cargo check` is red ([#163](https://github.com/ForrestThump/liberado/pull/163)); the files stay. F9 (#146), 0.1b (#147) and F12 (#156) were already on `main` and are now marked landed in the backlog. |
| **2026-08-11** | **Eight reviewed slices landed with green Ubuntu, Windows, dependency and docs checks.** ACP provider config now uses multi-tier resolution ([#137](https://github.com/ForrestThump/liberado/pull/137)); unattended goals lose `AskHuman` at the real grant boundary ([#138](https://github.com/ForrestThump/liberado/pull/138)); shepherd review labels now follow successful completion and use a 60-turn default ([#139](https://github.com/ForrestThump/liberado/pull/139)); exact MVL/execution-log contracts and conformance fixtures landed ([#140](https://github.com/ForrestThump/liberado/pull/140)); all three production coding surfaces use one assembler ([#141](https://github.com/ForrestThump/liberado/pull/141)); cold review is diff-bound and gets one reverified fix round ([#142](https://github.com/ForrestThump/liberado/pull/142)); termination preserves dirty headless work under meaningful task labels ([#143](https://github.com/ForrestThump/liberado/pull/143)); and daemon startup reconciles parked rows only after pack registration ([#144](https://github.com/ForrestThump/liberado/pull/144)). |
| **2026-08-11** | **Band 0, half of it.** **The ship bar now runs on the path that dispatches** ([#134](https://github.com/ForrestThump/liberado/pull/134), backlog 0.1): the preflight gate from #74 was reachable only through `CodingSessionPack`, which the ACP bridge does not use, so every dogfood run since Paseo landed skipped it. One decision now serves both paths — `ship_preflight_required_for` / `ship_spec_for` take a bare payload, and `ProjectConfig::ship_preflight_payload()` is the single builder the HTTP API and the bridge share. **The larger half of that PR was that the bridge loaded no config at all**: it read `LIBERADO_CONFIG_DIR` directly instead of `liberado_config::config_dir()`, and nothing set the variable, so every run read no topology, no policy and no tuning — no declared project, therefore no bar even with the gate wired, and an empty capability grant. The bridge now logs its resolved config dir and which files it found. **A success report requires a test run** ([#131](https://github.com/ForrestThump/liberado/pull/131), 0.2) — a run filed `succeeded` over seven failing tests because `cargo check` passed; the dangerous `git stash` + `git checkout` baseline in that contribution was replaced with a cached lookup. **An absent reviewer is not a verdict** ([#132](https://github.com/ForrestThump/liberado/pull/132), 0.3) — an empty critic response destroyed two finished runs; abstention is now `None`, never `Acceptable`. **A leaked env var is not a flake** ([#133](https://github.com/ForrestThump/liberado/pull/133)) — a Windows-only CI failure in an *unrelated* checkpoint test, caused by a sibling test clearing `GIT_CONFIG_GLOBAL` and deleting the file it named while a concurrent `git init` read it; fixing it exposed a real bug, since the no-translation setting was written only when creating a shadow repo and a **resumed** session restored with whatever the host had configured. |
| **2026-08-10** | **The trace gap, closed** ([#124](https://github.com/ForrestThump/liberado/pull/124)). `run_attempt` wrote its trace at four explicit return points and returned through a dozen `?` operators that were not among them — so an attempt ending in an *anticipated* way left a full record, and one ending in a way nobody anticipated left nothing at all. The write now wraps the body and happens on every exit path; `CoderEvent::SessionAborted` records the error text, distinct from `SessionFinished { outcome: Failed }` because a decision and the absence of one debug very differently. A failed trace write also no longer fails a completed run. Alongside it, [#125](https://github.com/ForrestThump/liberado/pull/125) fixed a genuinely flaky `narrow` property that compared element *order* for a set operation — `narrow_never_widens`, the property that would catch a real authority bug, passed throughout. |
| **2026-08-10** | **First substantial module written by the coding pack that survived review** — ACP file-backed session records ([#121](https://github.com/ForrestThump/liberado/pull/121), Paseo roadmap P3.1a): 492 lines and thirteen tests, atomic writes, untrusted session ids. Two hand fixes were needed and both are worth knowing: it **never ran the tests** (`cargo check` and `validate` pass without testing anything, and seven of its own tests failed), and it **reintroduced the process-global test race it had just built a mechanism to avoid**. Measured 2.3 reads per successful edit and **0 edit failures in 18 edits**, against a 66–70% failure baseline. Three harness bugs remain open, the first of which blocks trusting any of these numbers: **the trace is incomplete for multi-attempt runs** (122 tool calls on the wire, 76 in the traces). See [`coder-harness-reliability-2026-08.md`](future-work/coder-harness-reliability-2026-08.md). |
| **2026-08-10** | **Coding-harness reliability, PRs #106–#119** — fourteen defects, every one found by reading a failed run's trace rather than by review. Highlights: `write_file` silently destroying a file (#106); view-normalized edits + prompts moved out of the binary into `prompts/` (#107); hashline offered alongside raw-text edit tools, contaminating 14 of 41 anchors (#108); an error message that advertised its own bypass (#109); reviewers running on the coder's model instead of the configured critic (#110); warm-up so the worktree builds *before the first token* is spent, plus a shared build cache (#112); a documented TOML key serde never read (#113); `validate` answering `{"configured": false}` to a model asking the right question (#114); **`grep`** — regex, `output_mode`, context, glob, identifier-scored "did you mean" (#115); a Windows worktree-registry race (#116); **`model_request_sent`** so a trace records what the model was *sent*, system prompt included (#117); **untracked files are changes** — `git diff` shows tracked files only, so a model's own new module was invisible to it *and* to the critic (#118); and **infrastructure failures are not repairs** — a full disk was being classified as a code failure and retried (#119). Full record incl. failed hypotheses: [`coder-harness-reliability-2026-08.md`](future-work/coder-harness-reliability-2026-08.md). |
| **2026-08-09** | **ACP bridge Phase 0 + Phase 2 complete.** Tool-call id correlation (LIFO pairing so Paseo's tool UI attaches), `loadSession: false` chosen over a lying resume, `--version`/`--help` handled before stdin so probes cannot hang; coding/chat/face modes on one provider via `session/set_mode`, live OpenRouter catalog and `session/set_model`, cancel mid-turn. **TE3** (dispatcher prompt ordered stable-first) also verified landed. Roadmap rows corrected 2026-08-10 — several had sat open after the work shipped. |
| **2026-08-06** | **Hashline edit mode** (PR #76): configurable hashline edit mode for the coding harness. **Coverage gap analysis** (PR #75): mutant-test coverage across coder-* crates, test additions, clippy fixes for Rust 1.94. **Generic ship preflight gate** (PR #74): `PreflightRunner` + preflight block before `Succeeded` + self-PR quality ladder design. **Checkpoints + mid-build resume** (PR #73): shadow-git checkpoints per attempt + per write-flush, durable worktree park/resume, rewind — S4 landed. **Fan-out merge** (PR #72): hub-spawned coding children on worktree branches, parent LLM merge-back, max concurrent 3 — S6 v1 landed. **Self-host dogfood reliability** (PR #70/#71): no-changes-after-commit fix, live tool events, intake flexible decode, data-dir worktrees, `gh pr create --base` preflight. **Plan + explore PathPolicy presets** (PR #67/#68). **Project-root authorization** (PR #66). |
| **2026-08-03** | **Zone identity fix** (PR #38): a zone is identified by its *name*, not by which `Zone` variant spelled it. `Policy::write_class` always keyed on the name; `CapabilitySet` used derived structural equality, so a `Named` grant could never satisfy the write gate's `Zone::vault(..)` check — latent because nothing constructs `Named` yet, and waiting for the first non-vault CRUD surface. Serialization deliberately untouched (`Capability` is in `policy.toml` **and** the proposal HMAC). `tuning.md` gained the non-vault zone guide. **Seam boundary tests** (PR #39) and two read-only analysis tools: `delegation_cost.rs` and `provenance_ratio.rs` (examples then; promoted to `liberado-cost delegation-cost` / `provenance-ratio` in PR #63), which ranked the known seam conversation first at 29.4x against a median of 0.9x. **Evals decision** recorded in [`evals_implementation.md`](future-work/research/evals_implementation.md): no harness until there is a free oracle. |
| **2026-08-02** | **Round 2's five deliverables** (PRs #33–#37): **correlation coverage** (the cost instrument's own 8% blind spot, incl. the approval path); **turn-aware cost** + `token_usage_total` corrected from a lifetime sum to context occupancy; **Telegram parity** — `/model` scopes to the sticky chat, `/stop`, turn-lifecycle replies; **goal sessions in the shutdown drain**, parked durably on disk rather than left `Running`; and **Tier 3 P7 restart survival**, passing live. Every branch shipped a test claiming more than it checked — the four mechanisms and rules R6–R8 are in [round 3](future-work/parallel-deliverables-2026-08-round-3.md). |
| **2026-08-02** | **Five parallel deliverables** (PRs #28–#32), specced in [`parallel-deliverables-2026-08.md`](future-work/archive/parallel-deliverables-2026-08.md) and executed on separate branches: **token cost accounting** (`liberado-cost` — `[[models]]` per-million rates applied at *read* time over the existing latency journal, rolled up per conversation through the dispatch journal's `parent_conversation` so delegated spend lands on the turn that caused it; the full read landed 2026-08-02 at **92.8%** orchestrator — see [P1.5](#priority-15--token-economics-foundational-measured-2026-08-02)); **per-conversation compaction trigger** (closes CH4's re-resolve gap above); **TUI stop / scoped `/model` / reattach** (durable turns had made Ctrl+S mean "stop showing me"); **graceful shutdown** (SIGTERM drains in-flight durable turns for a bounded grace before exit; `stop_grace_period: 2m` on the compose service); and **Tier 3 P6**, verified against the live daemon. Review notes and the recurring test-aim weakness are written up in [`parallel-deliverables-2026-08-round-2.md`](future-work/archive/parallel-deliverables-2026-08-round-2.md). |
| **2026-08-01** | **Session profiles** (PR #21): per-conversation authority — a profile narrows tools, delegation, model, and prompt nudge for one conversation, and a turn may hold fewer capabilities than the profile's ceiling after per-goal narrowing. Includes **CH4's mechanism** (above), the **approval ledger** — a human's approve/reject now lives in an append-only log under `<LIBERADO_DATA_DIR>/` that no MCP mounts and no tool addresses, so editing a proposal note's `status:` authorises nothing — and the policy change that took `Write` on `proposals/` away from every agent grant. Plus the tool manifest (the model is told its exact tool surface each turn, beating stale transcript evidence), a provider-seam test sweep asserting on the serialized request body, and test-clock hardening (frozen state can no longer leak between tests, and the controls compile out of production builds). |
| **2026-07-23** | **CH3 context compaction:** long chats roll older history into a persisted summary marker (`Author::Named("compaction")` in the session DAG) — the model resumes from the summary + a verbatim tail of the last K user turns; the full transcript stays on disk, rendered and searchable. `[main_agent.compaction]` knobs, default on. Plan + 4-project research (OpenCode/Kilo/LibreChat/OpenClaw): [`context-compaction-plan.md`](future-work/context-compaction-plan.md). **Known residual** (marker durable + partial tail re-append → next load can miss unpersisted tail): documented there; preferred fix is CH3.1 viewport/side-summary — [`context-compaction-viewport-rearchitecture.md`](future-work/context-compaction-viewport-rearchitecture.md). |
| **2026-07-05** | **CH2 chat history search Tier 1** *(landed then, recorded now — the entry sat open here by mistake)*: lexical AND/regex over the session JSONL logs behind `GET /api/conversations/search`, the webui sidebar search box, and the `chat-search` MCP for the dispatcher. [`chat-search-plan.md`](future-work/archive/chat-search-plan.md). |
| **2026-07-23** | **C1 interactive crons (AskHuman):** profile-narrowed session grants — a cron schedule naming a `[[session_profiles]]` entry whose component includes `AskHuman` gets an open input channel; unattended crons stay structurally non-interactive (D-d). |
| **2026-07-23** | **M1b hot-reload** (`apply_mcp_peer_set` / `POST /api/mcp/reload`); **lock-poisoning recovery** (sessions + catalog); **proposal expiry reaper** (configurable, background); **vault path-traversal validation** (cross-platform); **MCP test double dedup** |
| **2026-07-23** | **Architecture hardening:** god-module splits; **MCP pooling** (M1); **M1b degraded-catalog routing**; **T1** L1–L10; **A4** dual-store hub tests |
| **2026-07-19** | TurboVault plugins live (vector + tasks); Telegram dogfood baseline |
| **2026-07-18** | Cron → Telegram delivery; OpenClaw brief cutover; sticky session persistence |
| **Earlier** | Unified Session (D7); one execution engine; `Write` at MCP boundary (F1); etc. |

See [`spec/architecture/sessions.md`](spec/architecture/sessions.md) for the session model history pointers.

**Last updated:** 2026-08-11.

> **A note on trusting this file.** On 2026-08-10 four items listed as open were found already
> shipped (ACP P0.1, P0.2/P0.3, P4.2, and TE3) — some for weeks. Items move to *Recently landed*
> only when someone remembers, and picking a task from a roadmap row without checking the code
> wastes a dispatch. **Verify against the code before you start.** If you find another stale row,
> correct it in the same pass; that is cheaper than the next person rediscovering it.
