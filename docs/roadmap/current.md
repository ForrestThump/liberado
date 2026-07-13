# Liberado — Roadmap

Forward-looking work, beyond what [`docs/architecture/overview.md`](../architecture/overview.md)
marks as built. Ordered loosely by priority within each section. This is a living doc — promote
items up as they're picked, and record *why* something is deferred so the reasoning isn't lost.

## Just landed — the unified Session model (2026-07-13)

`docs/architecture/sessions.md` is now the authoritative description; the slice-by-slice history is
in [session-focus-plan.md](session-focus-plan.md). In short: **everything is a `Session`** (D7), one
converged store, one list. Session profiles + `Capability::AskHuman`; intake-first coding sessions;
cron/hook/subagent runs recorded as **background sessions**; conversation **forking** (copy
semantics), including forking from any message in history.

### Known debt this opened — in priority order

1. **Two execution engines.** D7 unified how sessions are *stored and displayed*, not how they are
   *run*: the `GoalSessionHub` + `DomainPackRunner` packs run `/spawn`ed goal sessions, while the
   dispatcher + orchestrator run daemon reactions and `delegate`. That is why a background session's
   `domain` is recorded as `dispatch` and why joining one is **read-only** — no pack is hosting it.
   Routing unattended triggers through the hub as real packs is the convergence that closes this.
   *This is the largest structural debt in the system right now, and it was taken deliberately: the
   visibility was worth having before the convergence was.*
   **Sketched:** [one-execution-engine-plan.md](one-execution-engine-plan.md) — the dispatcher +
   orchestrator pair is already a `DomainPackRunner` in all but name, so this is a `DispatchPack`,
   not a third engine. Decisions pending before code.
2. ~~**Chat's tests don't run on the store chat actually uses.**~~ **Fixed 2026-07-13.** The
   `ConversationStore` conformance suite (14 invariants) now runs against `SessionStore`, and
   `JsonlStore` is **deleted** — `liberado-conversation-store` is the contract, with exactly one
   implementation. Doing this immediately caught **two more live defects** in `SessionStore` that no
   chat test could have found: the durable write was issued *outside* the lock its id was minted
   under (file order could disagree with id order), and it used `writeln!`, which can issue several
   `write` syscalls and let two appenders splice a line — which fails replay for the *entire
   session*. It also caught a test that had never tested what it claimed: the concurrency test used
   `#[tokio::test]` (single-threaded) + `join_all` (one task), so its 50 "concurrent" appends had
   always run strictly one at a time.
3. ~~**Packs record events, not turns.**~~ **Fixed 2026-07-13.** Packs now record dialogue as
   **turns** (`PackContext::record_turn` → `SessionRecordStore::append_turn` → a real node in the
   message DAG), keeping *events* for observations (a tool started, awaiting input). The kernel
   records the turns no pack should be able to forget: the goal opens the transcript, any human input
   is a turn by definition, and the outcome closes it. Both payoffs are live — a pack's Q&A is
   **searchable**, and a goal session is **forkable** (previously a 400). The coding pack records
   every question through one choke point (`ask`), so a new question cannot be added that silently
   fails to reach the transcript.

## The phased roadmap

The matured vision (see [Positioning](../architecture/positioning.md) and
[Overview's three pillars](../architecture/overview.md)) sequences the next work into four phases.
Each phase **ships value AND advances the modular substrate** — the substrate falls out of feature
work rather than being built as a months-long plumbing project up front (see Decision 18, the
incremental event-source/bus seams). The substrate work itself (config/policy, catalog, proposal
loop — Decisions 11/14/17) is largely landed; this roadmap is what's next. (Vocabulary note,
2026-07-11: "mesh" in older entries below means the kernel · domain packs · stores · surfaces
architecture — a star around one daemon — not peer routing; see
[contracts.md](../architecture/contracts.md) and the
[alignment audit](architecture-alignment-audit-2026-07-11.md).)

### Strategic pivot — Rust-native agentic orchestration (coding first)

Phase 2 proved the self-improvement moat's outer workflow: a coding task can become a draft PR behind
a human approval gate. That workflow briefly used `vtcode` as the coding harness; the live diagnosis
in [pr-dispatch-vtcode-no-write-finding.md](pr-dispatch-vtcode-no-write-finding.md) showed it is not
reliable. **We are not wrapping VTCode.** The direction is a first-party, home-spun **goal-oriented
agentic orchestration kernel** on `Provider` + `Executor` + `ToolRuntime`, with coding as the primary
domain and PR factory as the first consumer — keeping forge/task/approval pieces, replacing the
coding engine entirely.

Canonical architecture: [agentic-loops.md](../architecture/agentic-loops.md).  
Master plan: [rust-native-agentic-coder-plan.md](rust-native-agentic-coder-plan.md).  
Mesh hygiene audit: [agentic-mesh-hygiene-audit-2026-07-10.md](agentic-mesh-hygiene-audit-2026-07-10.md).  
Next interaction slice: [session-focus-plan.md](session-focus-plan.md) — interactive goal
sessions + UI focus switching ("specialist hats"), building directly on the converged event
vocabulary.  
Recurrence slice: [loops-plan.md](loops-plan.md) — *loops* (time-based series over goals; the
`/goal` vs `/loop` vocabulary was fixed 2026-07-12 in
[`agentic-loops.md`](../architecture/agentic-loops.md) §Vocabulary).

Short version: Liberado **owns** the coding engine. `coder-*` implement a coding **goal session**.
PR factory **defaults to `liberado-loop`** (`liberado-coder-run`); VTCode is legacy-only. Next:
live smokes, then a **coder eval layer in heuristics-tuner**. Same session/event direction backs
TUI/WebUI later; non-coding domains stay pack-shaped.

This is not a cosmetic agent swap. It advances kernel modularity, context efficiency, maker≠checker
critics, drift-resistant stop conditions, and empirical tuning (`heuristics-tuner` as the meta-loop).

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

### Before Phase 3 — heuristics tuning engine (dispatcher, executor, and subagent layers built — 2026-07-06)

Automate the manual "run eval → read the misses → tune the prompt → run again" loop
`liberado-eval` already documents doing by hand, so weak points in routing/tool-use surface
proactively instead of through slow dogfooding — the goal is a solid tool-use architecture
*before* Phase 3 autonomy breadth widens the surface area further. New crate
`liberado-heuristics-tuner` (using `provider-openai-compat`'s `openrouter_from_env()` — many
models behind one API/key, so concurrent evaluations aren't bottlenecked on one provider's rate
limit; this used to be its own `liberado-provider-openrouter` crate, collapsed into
`provider-openai-compat` since), local-search prompt tuning with
Monte Carlo restarts against local maxima, proposed prompt diffs reviewed by a human (never
auto-merged, same trust boundary as riggers' draft-PR pattern), plus a separate, lower-frequency
architecture-critique mode (not yet built). Started dispatcher-only; extended to the executor and
subagent layers (both score by actually driving a mocked `Executor::execute` tool loop, not just a
classification call), selectable via `tuner.toml`'s `layer`. A real elitism bug in the search loop
and a real engine bug (`REPORT_NUDGE`) were both found and fixed via this tool, live. Full design +
findings: [heuristics-tuning-engine-plan.md](heuristics-tuning-engine-plan.md).

- **✅ Multi-step tool chaining reliability — substantially resolved (2026-07-04).** Found via the
  tuner, not a tuner-specific problem — both `ExecuteDirect` and `DispatchSubagent` share the same
  execution engine. Root cause: a model can get stuck repeating one tool call with reworded-but-
  same-intent arguments, defeating byte-equality detection. Fixed with a doom-loop guard
  (`is_doom_loop`/`detect_short_cycle` in `liberado-executor`, TF-IDF argument similarity, not exact
  match) that escalates nudge → tool removal (with a one-time bounded turn-budget top-up) → honest
  failure, plus a progress-aware budget-exhaustion report. Live-verified 0/6 → 5/6 on the original
  failing scenario. One remaining gap (a fast-finish timing case, not a loop) and full evidence:
  [multi-step-execution-reliability-finding.md](multi-step-execution-reliability-finding.md).
- **✅ Resource-budget hardening (2026-07-04).** Ahead of Phase 3 widening the autonomy surface —
  cron introduces unattended, unprompted activation where a stuck run could go unnoticed far longer
  than one a human is watching in chat. The turn `Budget` used to be the only bound (no wall-clock
  or cost cap); generalized into a pluggable `ResourceLimit` trait (`liberado-executor`) so adding a
  new bounded resource later doesn't touch the loop's own logic. Wall-clock and a token-count proxy
  for cost (real `$`/token pricing deferred — not worth the upkeep while usage is this cheap) are
  wired in now; every existing `Budget::new`/`Budget::default` call site is unchanged.
- **✅ Zone-write-class guard (§6 #2) — done (2026-07-04).** See the "Landed" section below for the
  design (per-tool zone declarations with MCP-level inheritance, shared resolution helper between
  the dispatcher's pre-flight check and `RiskGatedToolRuntime`'s runtime enforcement).
- **✅ Proposal notifications (Telegram) — done (2026-07-04).** Closes the last "who's watching an
  unattended run" gap this hardening pass raised: a proposal written while nobody's looking at the
  vault (the exact case cron introduces) now also reaches a phone. New `liberado-notify` crate: a
  `Notifier` trait (Telegram is the first implementation — free, mature, works today; a future
  push-notification channel is a new impl, not a rewrite) wired into both real proposal-write sites
  (`Daemon::write_proposal`'s dispatcher pre-flight path, `RiskGatedToolRuntime::write_proposal`'s
  runtime path, reached via `Orchestrator`), all via `.with_notifier()` builder steps so no existing
  constructor call site needed to change. Always best-effort — a failed notification never blocks
  or fails the proposal write it's reporting on. Opt-in via `TELEGRAM_BOT_TOKEN` +
  `TELEGRAM_CHAT_ID` env vars (`TelegramNotifier::from_env()`); unset, nothing changes from before
  this existed. Live-verified: a real proposal write through the full `RiskGatedToolRuntime`
  production path actually delivered a Telegram message, not just the bare HTTP call in isolation.
- **✅ Two-way Telegram approval (approve/reject/revise) — done (2026-07-04).** Closes the follow-up
  question the notification above raised — can a human approve from their phone, not just find out
  about it? New `liberado-telegram-approvals` crate (`ApprovalBot`): a `getUpdates` poll loop that
  answers the Approve/Reject buttons `TelegramNotifier::notify_proposal` now sends (a new defaulted
  `Notifier::notify_proposal` method, Telegram-only override) with **pure code, no LLM** — a tap
  reads `proposals/{stem}.md`, flips `status`, and writes it back tagged `WriteProvenance::human()`
  (a new constructor), which the daemon's existing attribution/`handle_proposal_change` reacts to
  exactly like a human's Obsidian edit — no execution logic duplicated. Revise is the one
  LLM-touching path: taps into a `force_reply` prompt, hands the free-text note to the shared
  `Provider` (`complete_json` + a loose schema, same "the prompt carries the shape" precedent as the
  dispatcher's own) to redraft `rationale`/`proposed_action`, then **unconditionally re-signs** and
  writes back still `Pending` with fresh buttons — only a subsequent Approve tap (pure code) can ever
  execute anything, so an ambiguous or LLM-misjudged revision can never grant approval itself. Poll
  timing and the revise call's sampling temperature are config-file tunable (`tuning.toml`'s new
  `[telegram_approvals]` section, `TelegramApprovalsTuning`); credentials stay env-var only, never in
  a config file (Decision 10). Live-verified end-to-end with a real bot tap: the proposal note
  showed `status: done` with a matching integrity signature and the mock tool call recorded.

**Hardening pass complete.** All three gaps this pre-Phase-3 pass set out to close — the zone-write
guard, resource-budget bounds, and unattended-run visibility (now full round-trip approval, not just
a notification) — are done and live-verified. Combined with the already-fixed real bug and two
production-reachable panics the 2026-07-04 hygiene audit found (see
[hygiene-audit-2026-07-04.md](hygiene-audit-2026-07-04.md)), there's no known, named gap left
blocking Phase 3.

### Phase 3 — Autonomy breadth

Cron as a bus listener (Hermes gap #2, near-free) + the vault becomes the reactive event-source
plugin (vault-decoupling lands here, behind an event-source/hook trait). (Mesh checkpoint #3: cron and
vault-watch are interchangeable event-sources; a second dispatcher/executor is config-enableable.)
**✅ Checkpoint #3 fully done (2026-07-04)** — both halves, see below.

- **✅ Event-source trait + cron — done (2026-07-04).** The seam Decision 18/19 named: a new
  `EventSource` trait (`liberado-common`) that `Daemon::run` fans into one channel, reacting to
  whatever arrives regardless of source. Built in the explicit sequencing the user chose — the
  *existing* vault-watch loop was refactored into the trait's **first** conformer
  (`VaultEventSource`, `liberado-daemon`, moved not rewritten; the daemon's whole existing test
  suite passed unchanged, proving the seam is a true no-op) — before cron, its second conformer,
  was added. New `liberado-cron` crate (`Schedule`, `CronEventSource`) is deliberately
  vault-agnostic (no `liberado-vault` dependency at all) — the concrete proof Decision 19's
  "the core is vault-agnostic" claim is real. Config surface: `Topology.schedules` (parallel to
  `Topology.mcps`), each entry's `cron_expr`/uniqueness fail-fast validated (Decision 14). Cron
  reuses the existing `"dispatcher"` component/capability boundary rather than inventing a new one
  (v1 scope; a `component` field is the natural extension point if per-schedule scoping is ever
  needed). Live-verified in a daemon integration test asserting a cron firing and a real vault
  change both produce reactions over the same channel — the literal proof of Decision 18
  checkpoint #3 ("cron and vault-watch are interchangeable event-sources"). **Deferred, not
  dropped**: the generic external webhook/hook receiver (`Topology.hooks`, a config stub with
  nothing wired to it) and running a second, independently-scoped dispatcher/executor pool are
  separate, later slices.
- **✅ Named dispatcher/executor pools — done (2026-07-04).** Checkpoint #3's remaining half:
  multiple independently-authority-scoped dispatcher+executor pairs, routed by trigger. Before
  building, the user asked for outside research on whether concurrent-agent architectures like this
  are proven territory — a research prompt doc was written, sent to several external research
  models, and the results (`agent_pools_research_results.md`) converged hard: **internal**
  peer-agent authority-coordination is a poor, mostly-unproven fit (even Anthropic's own published
  multi-agent research system is strictly orchestrator + narrowed-workers, not peer coordination) —
  confirmed a bad fit, not just deferred. What this slice builds is the well-scoped piece the
  research didn't object to: pools that never talk to each other at all, each with its own named
  capability grant. `Daemon` now holds `pools: HashMap<String, DaemonPool>` (an always-present
  `"default"` entry keeps every pre-existing `with_dispatcher`/`with_orchestrator` call site
  unchanged — zero call-site breakage anywhere in the codebase). `EventPayload.pool` (set by
  `CronSchedule.pool`/`HookConfig.pool`, `None` ⇒ `"default"`) routes a trigger to its pool;
  `topology.toml`'s new `[[pools]]` declares one, validated fail-fast against schedules/hooks
  naming an undeclared or disabled pool. A pool's authority is nothing new — its name **is** the
  `component` key in `policy.toml`'s existing `[[grants]]`, the same mechanism `"dispatcher"`/
  `"main-agent"` already use. A privilege-escalation-shaped gap surfaced mid-implementation, not
  in the original plan: a proposal needs to remember which pool proposed it, or an approval could
  execute under a *different*, possibly broader pool's authority. Closed by making `Proposal.pool`
  a signed field (stamped by the proposing `Orchestrator`/`RiskGatedToolRuntime` before signing,
  re-verified defensively in `execute_approved`) — surfaced to the user as an explicit scope
  question (thread `pool_name` through ~10 call sites vs. document the gap) and resolved as "full
  fix now." Live-verified by a dual-pool daemon integration test: two pools given the identical
  decision referencing the same MCP, one granted it and one not — the ungranted pool's dispatcher
  guard (not just the orchestrator's own runtime scoping) catches the gap and never reaches a real
  runtime. **Deliberately out of scope** (research-confirmed): pools do not coordinate, communicate,
  or share state — that's the separate, genuinely open research question, not this slice. Also
  deferred, no concrete need yet: per-pool model/tuning override, per-pool concurrency budgets
  (there's no live concurrency budget to split today — `dispatch_parallel`'s semaphore isn't wired
  into the live react() path), zone-based vault-watch pool routing (vault-watch always uses
  `"default"`).
- **✅ External webhook hook receiver — done (2026-07-04).** The other half of "cron and hooks":
  `POST /api/hooks/{name}` (`crates/server/src/hooks.rs`), the push-style counterpart to cron's
  pull-style `EventSource` — arbitrary software that can `curl` an HTTP endpoint (systemd
  `ExecStart`, CI webhook steps, monitoring alerts, home-automation HTTP actions) triggers a
  reaction the same way a vault change or cron firing does. Required a small daemon change first:
  `Daemon::event_tx`/`event_rx` moved from being built fresh inside `run()` to daemon-owned fields
  (built once in `open()`), with a new `Daemon::event_sender()` accessor so an external, same-process
  producer (the HTTP handler) can inject an `Event` without needing its own `EventSource` loop —
  grabbed before `daemon.run()` consumes `self`, the same "grab a clone before the move" pattern
  the Telegram approval bot wiring already used for `daemon.vault()`/`daemon.signer()`.
  `ComponentConfig`/`Topology.hooks`'s old stub type was replaced in place with `HookConfig` (name,
  `secret_ref`, goal — parallel to `CronSchedule`), since the stub had no room for a secret and
  nothing else referenced it. Auth is a **per-hook shared secret** (`X-Liberado-Hook-Secret` header,
  constant-time compared) — the user's explicit choice over HMAC request signing, for the stated
  goal of being trivially `curl`-able from anything. Idempotency is an in-memory, TTL'd cache keyed
  on an optional `X-Liberado-Idempotency-Key` header — the original spec's persisted
  `.liberado/reactions/<id>.json` journal marker was never actually built anywhere in this codebase
  either, so this is the honest, consistent alternative, not a regression from a real mechanism.
  **Deferred, documented, not silently dropped**: in-process rate limiting (recommendation is a
  reverse proxy if this port is ever exposed beyond a LAN — consistent with this project's
  homelab/ssh-in-to-edit-config operational posture), HMAC signature verification as an available
  upgrade path, and per-hook capability scoping beyond the pool mechanism below (a hook can route to
  a named pool via `HookConfig.pool`, but there's no scoping *within* one hook's own single grant).
  Verified via 11 HTTP-level integration tests
  (`crates/server/src/hooks.rs`) exercising the full contract (right/wrong/missing secret, unknown
  hook, context merging, idempotent redelivery) against a real `axum::Router`; a live `curl`-based
  smoke test was attempted but skipped after a config-directory mixup in the test harness (caught
  before any request was sent — nothing touched) — the automated coverage was judged sufficient
  without repeating it.

### Phase 4 — Scaling

- **✅ Docker MCP transport — done (2026-07-07).** The v1 slice of Hermes gap #3 (execution
  environments). Confirmed while designing this that a Hermes-style `ExecutionEnvironment` trait
  *in the executor crate* would be the wrong layer here: `liberado-executor` is MCP-agnostic and
  owns no transport/connection concerns (Hermes' agent runs raw shell commands directly in a chosen
  backend; Liberado's agent only ever calls capability-gated MCP tools through `ToolRuntime`, so
  "which environment" really means "where does the MCP *server* process live" — a
  `crates/mcp`/`crates/bootstrap` connector concern). New `McpTransport::Docker { image, command,
  args, volumes, env }` (`crates/config-loader`) plus a `docker_argv` builder wired into
  `mcp_registry_from_config` (`crates/bootstrap`) — deliberately **no new connector type**:
  MCP-over-stdio doesn't care whether the child process is a bare binary or `docker run -i --rm
  image ...`, so the existing `StdioConnector`/`ChildProcessTransport` machinery (its `kill_on_drop`
  breaks the container's stdin, which a well-behaved MCP server exits on, and `--rm` removes it —
  no explicit `docker stop`/container-ID tracking needed) handles it unchanged. Isolation for a
  less-trusted or freshly-scaffolded MCP (e.g. one `riggers` just produced, not yet human-reviewed)
  is the concrete motivation, not "because Hermes has it." **Deferred, not built**: serverless
  hibernation (Modal/Daytona-style spin-to-zero) — no MCP today has an idle-cost problem that
  justifies the real cloud-backend integration this needs; and wiring `riggers` itself through this
  mechanism for per-task ephemeral sandboxing — the generic capability now exists, but `riggers`
  remains an externally-deployed long-running container today (`riggers/Dockerfile`), not something
  Liberado's own connector layer spawns. Full design:
  [phase-4-docker-transport.md](phase-4-docker-transport.md). **Not yet live-verified**: the Docker
  daemon wasn't running on the dev machine when this landed, so the actual `docker run` → MCP
  handshake path is unit-tested (config round-trip, `docker_argv`, registry registration) but not
  yet proven against a real container — see
  [human-todo.md](human-todo.md#phase-4-docker-mcp-transport--needs-a-live-smoke-test--2026-07-07).

## Landed (substrate already built)

- **Deterministic consequence guard (§6 #3)** — ✅ *done (per-MCP reversibility/externality)*. Gates
  direct action by `Consequence` (read-only < reversible < irreversible < external). Validated in
  `liberado-eval`: external actions (email/Slack) are deterministically downgraded to `Clarify` even
  at high confidence, while git-tracked vault writes flow.
- **Zone write-class guard (§6 #2)** — ✅ *done (2026-07-04, part of the Phase-3 hardening pass —
  see "Before Phase 3" below).* Downgrades an agent write targeting a `proposal_only`/`human_only`
  zone to a Proposal (Decision 11), on top of the consequence guard above. The missing piece this
  was deferred on ("tool→zone resolution") turned out to need new config surface, not just guard
  logic: `McpConfig` gained `default_zone`/`tools: Vec<ToolImpact>` (per-tool zone declarations,
  with unlabeled tools inheriting the MCP's `default_zone` — human-authored, like `consequence`,
  not self-declared by the MCP), and `liberado_common::McpDescriptor` carries the same
  `default_zone`/`tool_zones` so the dispatcher's pre-flight `guards.rs` and the runtime's
  `RiskGatedToolRuntime` share one resolution helper (`resolve_zone`/`resolve_declared_zone`) rather
  than duplicating it. Deliberately static per-tool declaration, not per-call argument
  introspection — a single generic multi-zone tool (e.g. `vault:write(path)`) needs distinct
  per-zone tool names to be discriminated by this guard; accepted as the simpler tradeoff over
  parsing arbitrary tool-specific argument shapes. Both the pre-flight and runtime layers fail safe
  to `WriteClass::ProposalOnly` for a resolved-but-unlisted zone, matching `Policy::write_class`'s
  own fail-safe default.
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
- ✅ **Dedup/coupling/decomposition/hygiene/coverage audit (2026-07-04)** — `cargo dupes` +
  `cargo llvm-cov` + 3 subagents across the whole workspace. Found one real bug (a failed proposal
  write in `RiskGatedToolRuntime` is silently reported to the user as success — the propose→approve
  loop's whole safety property depends on this), two reachable-in-production panics, and confirmed
  `provider-deepseek`/`provider-openrouter` (since collapsed into `provider-openai-compat`) were
  ~90% duplicated code. All Priority 1 items fixed same-session; the remaining Priority 2 backlog
  (heuristics-tuner module split, test extraction from oversized files, a couple of small dedups)
  closed out 2026-07-07 — see below. Full writeup:
  [hygiene-audit-2026-07-04.md](hygiene-audit-2026-07-04.md).
- **Hygiene audit + strategic reprioritization (2026-07-07)** — closed out three remaining
  `hygiene-audit-2026-07-04.md` backlog items (heuristics-tuner module split, test extraction from
  `tui/app.rs`/`main-agent/sessions.rs`, a `consequence_catalog` silent-fail-open log) plus stale
  doc references. Found one new Priority 1 item (a vault read failure in `build_event` is silently
  treated as "the file is now empty" rather than propagated) and two lock-poisoning landmines
  (low-probability, total-blast-radius). Concluded the project's hygiene discipline is genuinely
  healthy, not a stall tactic, and recommended Phase 4 (`ExecutionEnvironment`) as the next
  highest-leverage work — the one gap still open against `positioning.md`'s own competitive thesis.
  Full writeup: [hygiene-audit-2026-07-07.md](hygiene-audit-2026-07-07.md).
- ✅ **Catalog population** — done; this entry used to describe an open TODO ("the live daemon
  dispatches against an *empty* MCP catalog today") that was stale — `topology.mcps` has been the
  single source for both the dispatcher's catalog and the runtime's MCP connection since Phase 1.
  See [`../architecture/overview.md`](../architecture/overview.md)'s "Current status" item 9.
- ✅ **Runtime tool gating** — done, see "Per-call runtime enforcement" above (same item, noted twice
  in this doc).
- **Shared wire-type + slash-command extraction across clients** — TUI, WebUI, and CLI now share
  `chat-client-contract`'s `ChatEvent`/SSE decoder and `liberado-commands`' slash-command dispatcher
  instead of three hand-rolled copies. Full plan:
  [tui-shared-code-extraction-plan.md](tui-shared-code-extraction-plan.md) (decoder unification
  done). The plan's proposed `ChatClient` trait was deliberately **not** adopted — resolved
  2026-07-05 by deleting the never-implemented trait instead (`chat-client-contract` module docs
  now name `SseDecoder` + `ChatEvent::from_sse_data` as the real shared boundary; see
  `hygiene-audit-2026-07-05.md` P2.5).
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
- **Chat history search** — design captured, not scheduled:
  [`chat-search-plan.md`](chat-search-plan.md). Three tiers (lexical/ripgrep, BM25/`tantivy`,
  vector/semantic), shipped in that order, stopping whenever the simpler tier proves sufficient —
  only Tier 1 has a clear "just build it" case today. `liberado-conversation-store`'s per-conversation
  JSONL layout was already designed to "stay greppable," so Tier 1 is a near-free fit, not a
  repurposing.
