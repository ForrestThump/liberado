# Liberado vs Grok Build / OpenCode / Kimi Code — Agentic Coding Harness Gap Analysis

**Status:** research (architect roadmap input), 2026-08-05  
**Scope:** Agentic *coding* harness capabilities only — outer loops, isolation, tools, plan/verify, permissions, subagents, resume, coding surfaces. Not life-OS product market, billing, or multi-tenant SaaS.  
**Method:** In-tree sources for Liberado (`crates/*`, `docs/spec/architecture/*`, coding plans) and three FOSS clones (`grok-build/`, `opencode/`, `kimi-code/`).  
**Baseline rule:** **All open Liberado pull requests at analysis time are treated as already implemented** when judging Liberado capability (see [§2](#2-liberado-baseline--open-prs-as-landed)). Merged main includes PR #60 worktree isolation.  
**Non-edit note:** Older note [`ideas/vs-grok-build.md`](../ideas/vs-grok-build.md) overlaps Grok-only TUI framing; this document is the multi-harness, current-baseline deliverable and does not modify that file.

---

## 1. Executive summary (architect one-pager)

### What Liberado already is (coding)

Liberado is a **general agentic orchestration kernel** (goal sessions, budgets, terminals, capability zones, dual-store events, multi-surface clients) with a **first domain pack** for coding (`coder-*`). Coding is deliberately *not* the center of gravity of the product — but the coding pack is real:

| Strength | Why it matters for coding performance |
|---|---|
| **Goal-session kernel** | `/goal` surface, hub park/resume/cancel, SSE stream, named terminals — outer loop is a first-class object, not chat prose |
| **Maker ≠ checker** | Intake-frozen criteria, deterministic verifiers, critic on real git evidence, optional multi-reviewer **completion gate** (gatekeeper + fresh quorum + strategist) |
| **Capability / zone model** | Narrow-only delegation, risk-gated tools, proposals + approval ledger — stronger trust story than “ask/yolo” alone |
| **Worktree isolation (main + #60)** | Coding sessions on git repos get `WorktreeWorkspace` — isolation **before** fan-out (the Bun race lesson is already half-applied) |
| **Durable multi-surface** | Daemon + TUI + HTTP/SSE + Telegram; joinable goals, AskHuman, shutdown park of in-flight goals |
| **Coding tool core** | Files, search, patch, git (status/diff/branch/commit/push), shell, validate; **`list_symbols`** (open PR #62) |

### Where Liberado is behind purpose-built coding harnesses

| Gap class | Liberado today | FOSS reference |
|---|---|---|
| **Time-based series (`/loop`)** | Designed (`loops-plan.md`); **zero production code** | Grok: `/loop` + scheduler tools; Kimi: session cron tools; OpenCode: external only |
| **Agent-reachable parallelism** | `dispatch_parallel` **built but unwired**; tools **serial**; `delegate` **sync await** | Grok/Kimi/OpenCode: multi-subagent + (often) parallel tool settle |
| **Coding subagent productization** | Dispatch-pack children; **no** coding worktree children with merge-back | Grok `task` + isolation; OpenCode `task` + child sessions; Kimi Agent/Swarm |
| **Plan mode as UX/FSM** | Planner role + capability narrowing exist; **no** exclusive plan-file mode + approval UI | Grok/OpenCode/Kimi all ship plan mode |
| **Workspace rewind / checkpoints** | Park session yes; **mid-build resume false**; no shadow-git `/rewind` | Grok FS checkpoints; OpenCode snapshot/revert |
| **Interactive coding chrome** | Goal sidebar (#61) helps; still thin on diffs, permission modes, task dashboard, headless `-p` | Grok/OpenCode/Kimi product UIs |
| **Repo intelligence** | Flat list + grep + `list_symbols` (#62) | Grok graph+LSP; OpenCode LSP/grep/glob; Kimi full tool set |
| **Skills / plugins / hooks market** | Topology, MCP, prompts — not agent skill marketplace | All three FOSS ecosystems stronger |

### Where Liberado is ahead (do not regress)

- **Domain-agnostic goal kernel** (life + coding share hub vocabulary).
- **Deterministic verifier pipeline** as success authority (OpenCode core lacks this; Grok/Kimi verify more via model panels / culture).
- **Life-OS composition**: cron daemon, Telegram, vault, multi-MCP mesh, PR-dispatch farms — not coding-only.
- **Fail-closed capability topology** (pools, profiles, grants) rather than only session permission modes.

### What “perform as well as these harnesses” means for Liberado

Not “clone Grok Build’s TUI.” It means, for **coding jobs of comparable difficulty**:

1. **Outer success loop reliability** — keep working until evidence-backed terminal, with stall recovery and human-visible state (parity: Grok/Kimi `/goal`; Liberado has the bones, must finish surface + gate dogfood + mid-build resume).
2. **Safe width** — independent exploration/edits in **isolated workspaces**, then merge; tools and subagents may run concurrent without trampling (parity: Grok worktree isolation + parallel tools; OpenCode child sessions + worktrees; Kimi swarm — Liberado has isolation primitive, not the fan-out product).
3. **Interactive control plane** — plan → approve → implement; permission modes that match coding cadence; rewind when the agent digs a hole (parity: all three).
4. **Recurring improvement** — series memory over goals (`/loop`), not only one-shot cron (parity: Grok/Kimi; Liberado plan is strong, unbuilt).
5. **Repo orientation speed** — structural overview + navigation so the model wastes fewer turns on `list_files` thrashing (parity: FOSS tooling depth).

### Roadmap shape (recommended bands)

| Band | Theme | Closes gap vs |
|---|---|---|
| **A — Finish goal product** | Gate default-on dogfood, project auth, goal panes/diff UX, mid-build resume, plan mode profile | Grok/Kimi `/goal` *feel* |
| **B — Isolation → width** | Wire `dispatch_parallel` / async coding subagents on worktrees + merge-back; optional parallel tools | All three parallelism |
| **C — Series loops** | Implement `loops-plan.md` P1–P4 | Grok `/loop`, Kimi cron |
| **D — Coding chrome + tools** | Checkpoints/rewind, plan approval UI, headless coding CLI, LSP/symbols depth, skills discovery | OpenCode/Grok product depth |
| **E — Measure** | Gate/strategist evals, coding curriculum, token economics (TE1 catalog) | Performance honesty |

---

## 2. Liberado baseline — open PRs as landed

Analysis inventory (`gh pr list --state open`, 2026-08-05). **Every open PR below is counted as present capability** (partial-merge / draft risk noted, not reclassified as a gap).

| PR | Title | Capability counted as shipped |
|---|---|---|
| **#60** *(merged on main)* | `feat(coder-agent): apply worktree isolation to all coding sessions` | Coding sessions on git repos use `WorktreeWorkspace` (not only default-temp path). Isolation unblocks future fan-out; `dispatch_parallel` still not agent-reachable. |
| **#61** | `feat(tui): add goal session sidebar with live gate votes` | Dedicated goal sidebar; live gate-vote accumulation instead of only scrolling system lines |
| **#62** | `feat(coder-tools): add list_symbols tool for codebase orientation` | Structural symbol listing for orientation (beyond flat `list_files` / `search_text`) |
| **#63** | `feat(cost): promote provenance_ratio and delegation_cost to subcommands` | Cost analysis tools as first-class `liberado-cost` subcommands (ops visibility for coding spend) |
| **#64** | `fix(main-agent): verify face-agent prompt ordering for cache reuse` | Face-agent static-before-varying prompt order locked by test (cache hit rate / token economics) |
| **#2** *(draft, auto-generated)* | WebUI chat HTML edit (subagent draft) | Treated as present WebUI surface work; **draft** — capability thin; do not treat as full goal WebUI |

**Note:** PRs #61–#64 (and draft #2) were **open and un-merged** at analysis time. They are counted as landed per the baseline rule above, which assumes near-term merge of the current Band C/D work. The aggregate parity assessment is therefore forward-looking on these items. The text marks open-PR capability with the PR number so a reader can verify merge status independently.

**Main tip at branch cut:** `a902520` — *Merge PR #60 — C7: worktree isolation for all coding sessions*.

### Since the analysis was cut (added on merge, 2026-08-05)

The forward-looking bet paid off for the inventory above — **#61, #62, #63 and #64 all merged**, each after review fixes. Three PRs opened *after* the `gh pr list` snapshot, and one of them moves a row this document treats as a gap:

| PR | Effect on this analysis |
|---|---|
| **#66** | Project-root authorization for coding goals (S3/G4) — Band A's "project auth" line. |
| **#67** | **Plan mode via `PathPolicy` / `CommandPolicy` presets.** §1 lists "Plan mode as UX/FSM" as a gap and Band A calls for a "plan mode profile"; treat that row as in-flight rather than absent. Note the shape differs from the FOSS references — a policy preset, not a separate FSM, which is the cheaper answer if it holds. |
| **#68** | Explore mode as a read-only `PathPolicy`/catalog preset — same mechanism, no equivalent row here. |

Nothing else in the analysis is affected: the parallelism, series-loop, checkpoint and repo-intelligence gaps are all untouched by those three.

### Doc drift warning

Some living docs still claim “no `WorktreeWorkspace`” or “no `/api/goals/{id}/diff`”. Code on main has both (`crates/coder-sandbox`, goals API). This analysis prefers **code + open PRs** over stale roadmap rows.

---

## 3. Product frame (do not confuse jobs)

| | **Liberado** | **Grok Build** | **OpenCode** | **Kimi Code** |
|---|---|---|---|---|
| **Job** | Life-OS daemon + multi-domain agents; coding is pack #1 | Terminal coding agent product | Multi-surface coding agent product | Multi-surface coding agent product |
| **Loop owner** | Kernel hub + packs; TUI is client | In-process shell/session actor | Server + session runner; TUI/desktop clients | CLI/core process + kap-server |
| **Default UX** | `liberado serve` + TUI/Telegram over HTTP/SSE | `cd repo && grok` | `opencode` in project | `kimi` in project |
| **Trust model** | Capabilities, zones, profiles, proposals | Permission modes + sandbox + plan mode | allow/ask/deny + plan agent | manual/yolo/auto + plan + policy chain |
| **Outer success loop** | Goal sessions (kernel) | First-class `/goal` + skeptic panel | Session loop; `/goal` community plugin | First-class `/goal` + budgets/queue |
| **Time recurrence** | System cron + **planned** series loops | `/loop` + scheduler tools | External CI/plugins | Session cron tools |

Liberado should **steal harness patterns**, not become a fourth coding-only product. The parity bar is: *when coding, the pack + surface must not feel half-finished next to FOSS harnesses.*

---

## 4. Parity matrix (agentic coding surface)

Legend: **Y** = productized / agent-reachable · **P** = partial / opt-in / tests-only / plan · **N** = absent or ecosystem-only · **—** = N/A by design

### 4.1 Outer loops & recurrence

| Capability | Liberado | Grok Build | OpenCode | Kimi Code |
|---|---|---|---|---|
| Success-driven outer loop (`/goal`) | **Y** (kernel + slash + API; gate opt-in) | **Y** (planner → worker → adversarial verify) | **N** core (plugin) | **Y** (driver + tools + queue) |
| Frozen acceptance criteria | **Y** (intake / `GoalContract` / verifiers) | **Y** (contract culture + plan file) | **P** (plan/todos informal) | **P** (proof-of-done culture + tools) |
| Multi-attempt repair | **Y** (`coder-agent` attempts + feedback) | **Y** | **P** (steps budget) | **Y** (continuation while active) |
| Independent completion verification | **Y** (verifiers + critic + optional gate) | **Y** (skeptic panel, fail-closed) | **N** harness-level | **P** (model re-judge; bash tests) |
| Goal pause / resume / status UX | **Y** (`/goal` + park/resume API; sidebar #61) | **Y** | **—** | **Y** |
| Goal queue | **N** | **P** | **N** | **Y** (`/goal next`) |
| Time-based `/loop` or series | **P** (design only) | **Y** (`/loop`, scheduler_*) | **N** (external) | **Y** (session cron tools) |
| System cron (daemon) | **Y** (life-OS strength) | **P** (session scheduler) | **P** (GitHub Action) | **P** (session-bound) |

### 4.2 Isolation & parallelism

| Capability | Liberado | Grok Build | OpenCode | Kimi Code |
|---|---|---|---|---|
| Per-session git worktree | **Y** (#60 / `WorktreeWorkspace`) | **Y** (`xai-fast-worktree`, apply/GC) | **Y** (experimental worktree API) | **N** (detect only) |
| Subagent worktree isolation | **P** (isolation exists; coding child product open) | **Y** (`isolation: worktree`) | **P** (workspace/worktree control plane) | **N** |
| Parallel subagent fan-out | **P** (`dispatch_parallel` built; no agent path calls it) | **Y** (task + wait_tasks + workflows) | **Y** (task / background experimental) | **Y** (Agent + AgentSwarm) |
| Parallel tool calls (one turn) | **N** (serial `for` loop) | **P** (flagged parallel dispatch) | **Y** (eager settle) | **Y** (resource-aware scheduler) |
| Merge-back story | **P** (worktree Drop/cleanup; no product apply) | **Y** (apply into main) | **P** | **—** |

### 4.3 Plan, permissions, verify

| Capability | Liberado | Grok Build | OpenCode | Kimi Code |
|---|---|---|---|---|
| Plan mode FSM + exclusive plan file | **P** (planner role / profiles) | **Y** | **Y** (build/plan agents) | **Y** |
| Interactive permission modes | **P** (caps + proposals; not Shift+Tab coding modes) | **Y** | **Y** | **Y** (manual/yolo/auto) |
| OS sandbox (Landlock/Seatbelt/etc.) | **P** (Docker scaffold; host/worktree default) | **Y** | **P** (community containers) | **P** (kaos local/SSH) |
| Deterministic verifiers-as-code | **Y** | **P** | **N** | **N** core |
| Multi-reviewer completion gate | **Y** (default **off**) | **Y** (core to `/goal`) | **N** | **N** (different model) |

### 4.4 Tools, context, continuity

| Capability | Liberado | Grok Build | OpenCode | Kimi Code |
|---|---|---|---|---|
| Core edit/search/shell | **Y** | **Y** | **Y** | **Y** |
| Git commit/push tools | **Y** | **Y** (workspace VCS) | **Y** (bash-primary) | **Y** (bash + policies) |
| Symbol / graph / LSP | **P** (`list_symbols` #62) | **Y** (graph + LSP) | **Y** (grep/glob + experimental LSP) | **P** (grep/glob; no product graph) |
| Web search/fetch | **P** (MCP / life tools; not coding pack default) | **Y** | **Y** | **Y** |
| Skills / plugins / hooks | **P** (MCP + topology) | **Y** | **Y** | **Y** |
| Chat/session compaction | **Y** (chat); goal-turn compaction **P** | **Y** | **Y** | **Y** |
| FS checkpoints / rewind | **N** (S4 plan) | **Y** | **Y** (snapshot/revert) | **N** (conversation undo) |
| Mid-build coding resume | **N** (`can_resume` false after coder role) | **Y** | **Y** (session continue) | **Y** (session; goal → paused) |
| Headless one-shot coding CLI | **P** (API/evals/PR factory) | **Y** (`grok -p`) | **Y** | **Y** (`kimi -p`, goal exit codes) |
| ACP / editor protocol | **N** planned | **Y** | **Y** | **Y** |

### 4.5 Surfaces

| Capability | Liberado | Grok Build | OpenCode | Kimi Code |
|---|---|---|---|---|
| Coding-first TUI | **P** (session client + goal sidebar #61) | **Y** | **Y** | **Y** |
| Desktop / multi-app | **P** (WebUI chat-first) | **P** | **Y** | **Y** |
| Multi-session dashboard | **P** (switcher) | **Y** | **P** | **P** |
| Remote life surfaces (Telegram, etc.) | **Y** | **N** | **P** (Slack package) | **P** |

---

## 5. `/goal` — success-driven outer loops

### 5.1 Liberado (baseline = main + open PRs)

**Architecture** ([`agentic-loops.md`](../../spec/architecture/agentic-loops.md)):

```
turn loop (executor) ⊂ goal (session hub + pack) ⊂ loop (planned) ⊂ meta-loop (tuner)
```

| Layer | Status | Evidence |
|---|---|---|
| Hub lifecycle | Shipped | `crates/session` — start, park, resume, cancel, stream, grants |
| Coding attempt loop | Shipped | `crates/coder-agent` — intake → (plan) → worker → verifiers → critic/gate → repair |
| Slash UX | Shipped | `liberado-commands` + TUI: `/goal`, `/goal in <project>`, status/pause/resume/clear |
| Completion gate | Shipped, **default off** | Kernel `completion_gate` + coding adapter; gatekeeper + fresh quorum + strategist |
| Surface chrome | Partial → **#61 as present** | Sidebar + live gate votes; dedicated role/verifier widgets still thin |
| Project authorization | Open (coding-tui S3) | `workspace_root` still needs `[[projects]]` fail-closed allowlist |
| Diff API | Shipped on main | `GET /api/goals/{id}/diff` |

**Performance posture vs FOSS:** Liberado’s **philosophy is stronger than OpenCode** (criteria + verifiers + maker≠checker). Vs **Grok Build**, Liberado has the same *shape* (disputed completion, strategist) but:

- Gate remains **opt-in** (cost: `1 + fresh_reviewers` model calls per attempt).
- Grok adds **premature-stop detectors**, **laziness classifiers**, and richer pause taxonomy wired into a coding-first TUI.
- Kimi adds **goal queue**, **hard budgets with headless exit codes**, and **main-only goal tools** so subagents cannot corrupt goal state.

### 5.2 Grok Build

- `/goal <objective> [--budget]` with phases Planning/Executing and statuses (Active, various Paused, Blocked, BudgetLimited, Complete).
- Planner subagent writes plan; host-driven evaluation; adversarial skeptic panel; stall fingerprints; strategist bonus runs.
- Model tool `update_goal`; stop-phrase nudges; live GoalUpdated notifications.
- Paths: `xai-grok-shell` `goal_tracker.rs`, `goal_evaluator.rs`, `goal_planner.rs`; tools `update_goal/`; user-guide `04-slash-commands.md`.

### 5.3 OpenCode

- **No first-party `/goal`.** Core is conversation + step budgets + todos + plan agent.
- Community `opencode-goal-plugin` only.
- Implication: Liberado must **not** regress goal-as-kernel to “just keep chatting.”

### 5.4 Kimi Code

- Spec `GOAL.md` + production guides; tools Create/Get/Update/SetBudget; continuation driver while `active`.
- Queue: `/goal next`, manage.
- Resume safety: restored active → **paused** (no stealth re-run).
- Headless: exit codes for complete/blocked/paused.

### 5.5 Gap list + what needs to happen (Liberado)

| # | Gap | What needs to happen |
|---|---|---|
| G-A1 | Gate default-off | Dogfood S7: measure cost/quality; enable gate for coding profiles or “strict” goals; keep opt-out |
| G-A2 | Live gate UX incomplete | Land #61 (counted); finish role timeline + verifier panel; stream votes live through pack if still batched |
| G-A3 | Project root auth | S3: `[[projects]]` + API list + fail-closed undeclared roots |
| G-A4 | Mid-build resume | Checkpoint workspace + pack `can_resume` after coder role; design E6-c(b) |
| G-A5 | Goal queue | Optional: queue next objectives (Kimi pattern) on hub without new engine |
| G-A6 | Premature bail | Add stop-phrase / no-progress classifiers at goal layer (Grok pattern) on top of existing progress guards |
| G-A7 | Headless goal CLI | `liberado goal -p` (or serve-less runner) with exit codes for CI/self-improvement |

---

## 6. `/loop` — time-based / series flows

### 6.1 Liberado

| | |
|---|---|
| **Status** | **Plan only** — [`loops-plan.md`](../loops-plan.md) (2026-07-12): *“no code yet”* |
| **Design decision** | A loop is a **scheduler for goals**, not a fourth engine. Body = ordinary goal session. Series state + changelog under data dir. |
| **Already reusable** | Cron schedules, goal hub, verifiers as checkers, notify, Decision-5 loop-break (artifact edits safe) |
| **Missing** | `[[loops]]` config, `LoopSeries` durable state, runner, pass context assembly, `/api/loops*`, TUI list/changelog, `ProposeLoop` |

Vocabulary (agentic-loops): turn ⊂ goal ⊂ **loop (designed)** ⊂ meta-loop.

**Related but not equivalent:** `liberado-cron` one-shot firings lack **series memory** (each fire is amnesiac). That series memory is the entire `/loop` gap.

### 6.2 Grok Build

- `/loop [interval] <prompt>` — interval units s/m/h/d; max 50 tasks; 7-day auto-expire.
- Tools: `scheduler_create` / list / delete; `durable` across sessions; `monitor` for long-running shells.
- Wakes idle agent turns — session-centric, not life-OS daemon.

### 6.3 OpenCode

- No in-process `/loop`. GitHub Action schedules + community `opencode-scheduler`.

### 6.4 Kimi Code

- Session tools `CronCreate` / List / Delete; 5-field cron; idle-gate; jitter; ≤50 tasks; 7-day stale delete.
- Not a free-standing global daemon; survives session resume only.

### 6.5 Gap list + what needs to happen

| # | Gap | What needs to happen |
|---|---|---|
| G-L1 | No series type | Implement loops-plan **P1**: config shape + `LoopSeries` + changelog JSONL |
| G-L2 | No runner | **P2**: cron fire → spawn goal → append pass → stop_when (green streak / cap / human close); skip-on-overlap |
| G-L3 | No surface | **P4**: `/api/loops*` + TUI; pass is ordinary goal (reuse stream) |
| G-L4 | No agent propose path | Later: `ProposeLoop` via proposal flow (Decision 14) |
| G-L5 | Naming | Ship `/loop` slash for series (match FOSS convention); keep system cron for life-OS |

**Parity target:** Grok/Kimi *session* recurrence + Liberado *daemon* durability. Liberado’s design is the most coherent long-term; it just needs code.

---

## 7. Parallel worktree (isolation) dispatch

### 7.1 Liberado

**Shipped (main #60):**

- `WorktreeWorkspace` in `crates/coder-sandbox` — `git worktree add`, checkout HEAD, Drop cleanup, path containment tests.
- Coding pack `session_pack/build.rs`: **Worktree if workspace is a git repo, else HostLocal**.
- Tools/verifiers operate on effective worktree root.

**Still open (architecture rule: isolation before parallelism is *half* done):**

| Piece | Status |
|---|---|
| `Orchestrator::dispatch_parallel` | Built + tested; **no agent path calls it** (no fan-out `DispatchAction`) |
| Fan-out `DispatchAction` | Missing — classifier cannot say “these are independent” |
| Agent `delegate` | `start_background` + **`await_terminal`** (synchronous in chat turn); AskHuman stripped |
| Parallel tools in executor | Serial `for call in tool_calls` |
| Coding subagents with merge-back | Open (coding-tui S6 / backlog C7) |
| Worktree apply-into-parent UX | Not productized (cleanup yes; merge story no) |

Consequence: Liberado is still **“protected by accident”** from multi-writer races — nothing fans out. Closing fan-out without merge/isolation discipline reintroduces the Bun class of failure.

### 7.2 Grok Build

- Production worktrees: CoW / BTRFS / pools / GC (`xai-fast-worktree`).
- Subagent `isolation: none | worktree`; session `/fork --worktree`; CLI `grok -w -r`.
- Explicit **apply** of worktree edits into main tree.
- Parallel background subagents + optional parallel tool dispatch.

### 7.3 OpenCode

- Experimental HTTP worktree create/list/remove/reset; control-plane workspace adapters; session warp.
- Child sessions via `task` tool; within-turn parallel tool settle; multi-session concurrent coordinator.

### 7.4 Kimi Code

- **No** product “spawn in worktree.” Parallelism is **Agent / AgentSwarm** sharing workspace FS (plus extra dirs / SSH kaos).
- Tool scheduler allows non-conflicting parallel tool resource access.

### 7.5 Gap list + what needs to happen

| # | Gap | What needs to happen |
|---|---|---|
| G-W1 | Unreachable parallel dispatch | Add fan-out action + classifier decomposition **after** merge design is specified |
| G-W2 | Coding child sessions | Pack-native coding subagent: own worktree, narrowed tools, single Report + patch/merge-back |
| G-W3 | Async delegate | Non-blocking delegate variant (hub already supports human `/spawn` handoff) |
| G-W4 | Tool concurrency | Optional JoinSet for **read-only / non-conflicting** tools only; never bare concurrent writers on one workspace |
| G-W5 | Merge protocol | Answer: where each worker works, how results merge, conflict policy — then implement apply |
| G-W6 | Teardown | Wire `WorktreeWorkspace::cleanup()` on session teardown (Drop helps; explicit prune on success path) |
| G-W7 | Fast worktrees (optional) | Only if scale demands: CoW/pool (Grok) — not required for correctness |

---

## 8. Other material harness capabilities

### 8.1 Coding tools

| Liberado (`coder-tools`) | FOSS extras to consider |
|---|---|
| list_files, search_text, read/write/edit, apply_patch | OpenCode `glob` + ripgrep UX; Grok hashline anchors |
| git_status/diff/branch/commit/push | Keep; policy-harden push |
| run_command, validate | OpenCode formatters post-edit |
| **list_symbols (#62)** | Next: tree-sitter depth, go-to-def (Grok graph / OpenCode LSP) — not full reimplementation day one |

**Rough edge:** Web tools live in MCP mesh, not coding pack — fine for life-OS, slow for “research this API while coding.”

### 8.2 Plan mode

| System | Model |
|---|---|
| Liberado | Optional planner; capability profiles *can* implement plan-as-read-only — **not a first-class mode** |
| Grok | Plan FSM; only plan.md writable; approval UI with line comments |
| OpenCode | Primary `plan` agent vs `build`; Tab switch; plan_exit confirmation |
| Kimi | Enter/ExitPlanMode; write sandbox; review card |

**What needs to happen:** A `plan` session profile (or grant template) that denies write/shell except plan artifact; TUI mode cycle; exit → human approve → build grant. Mechanism is mostly **config + UX**, not a new kernel.

### 8.3 Permissions & sandbox

| Liberado strength | FOSS strength |
|---|---|
| Zones, pools, profiles, proposals, approval ledger | Fast interactive modes (ask / acceptEdits / yolo / auto) |
| Risk-gated high-consequence → proposal | Per-tool pattern rules (OpenCode), policy chains (Kimi) |

**Rough edge for coding cadence:** Telegram/proposal flow is correct for unattended life-OS; for interactive TUI coding it feels heavy. Need **coding-local permission modes** that still **cannot widen** kernel capability ceilings.

### 8.4 Subagents & multi-agent

| Liberado | FOSS |
|---|---|
| Face `delegate` → dispatch pack; capability ∩; sync | Typed agents (explore/plan/coder), depth limits, swarm, resume_from, background tasks |
| Parallel API unusable | Productized dashboards |

### 8.5 Resume, checkpoints, compaction

| Mechanism | Liberado | Need |
|---|---|---|
| Park goal across restart | Y | Keep |
| Mid-build coding resume | N | Shadow git or worktree snapshot per attempt |
| `/rewind` file restore | N | OpenCode snapshot or Grok checkpoint store |
| Chat compaction | Y | Extend to long goal turn loops |
| Cost / cache ordering | #64 + TE track | Finish catalog narrowing (TE1) — 56% spend is orchestrator base context |

### 8.6 Surfaces (TUI / CLI / ACP)

| Liberado | Gap vs FOSS |
|---|---|
| Solid session client; join/AskHuman; `/goal`; sidebar #61 | Diff review pane, plan approval, tasks dashboard, permission footer, context meter |
| CLI: serve/chat | Headless coding `-p` |
| WebUI chat | Goal surface (W1 roadmap) |
| No ACP | After TUI is good enough |

### 8.7 Extensibility (skills / MCP / hooks)

Liberado’s MCP topology is a **platform strength**. Missing for coding *ergonomics*:

- Project `AGENTS.md` / skill folder discovery as **coding pack perceive** (PR factory already has a pattern).
- Hooks at tool/stop boundaries for format/test gates (Grok/OpenCode/Kimi).
- Optional skill slash commands (without building a marketplace first).

---

## 9. Rough edges (shipped but sharp)

These are not “missing features”; they are **performance and reliability debt** that FOSS harnesses either avoid or productized around:

1. **Gate cost vs quality** — multi-reviewer gate off by default; without it Liberado looks closer to “critic once”; with it, token burn rises.
2. **Serial tools** — multi-tool model turns wait in line; latency and “feels dumb” next to OpenCode/Kimi parallel settle.
3. **Sync delegate** — multi-hop coding research cannot overlap; face turn blocked.
4. **Token economics** — orchestrator ~11k base context re-sent (TE1); face protection not paying off in measurements.
5. **Worktree without apply UX** — isolation without merge productization means parallel workers (when added) still lack a user story.
6. **Can_resume false after build starts** — long coding jobs are not restart-safe mid-diff.
7. **No series memory on cron** — recurring vault/doc improvement cannot learn from previous passes until loops land.
8. **Doc/code drift** — some living docs and older research notes still reference coding tools or isolation state that has since landed; implementers may waste time chasing stale claims. Prefer code + §2 open-PR baseline over older doc claims.
9. **VTCode/PR-dispatch historical path** — external PR factory findings; keep coding pack path pure.

---

## 10. Feature parity scorecard (honest)

Relative to **coding harness job** (not life-OS):

| Area | Liberado vs best of FOSS | Verdict |
|---|---|---|
| Goal kernel / criteria / verifiers | Competitive or **ahead** (esp. vs OpenCode) | **Parity-capable**; dogfood gate + resume |
| Goal UX / queue / bail guards | Behind Grok/Kimi | **Gap** |
| `/loop` series | Behind Grok/Kimi; design ready | **Major gap** |
| Worktree isolation primitive | On par with OpenCode; behind Grok polish | **Parity** (primitive) |
| Parallel dispatch product | Behind all three | **Major gap** |
| Plan mode UX | Behind all three | **Gap** |
| Interactive permission UX | Behind all three | **Gap** |
| Coding tool depth (LSP/graph/web) | Behind Grok/OpenCode | **Gap** |
| Checkpoints/rewind | Behind Grok/OpenCode | **Major gap** |
| Multi-surface coding apps | Behind OpenCode/Kimi | Accept or W1 later |
| Skills/plugins marketplace | Behind all three | **Medium** (steal discovery, not market) |
| Capability/trust topology | **Ahead** | Preserve |
| Daemon / multi-domain / Telegram | **Ahead** | Preserve |

---

## 11. Architect roadmap implications (ordered)

Priority is for **coding harness performance + parity**, assuming life-OS P1 continues in parallel.

### Band A — Goal product completion (highest leverage)

1. **Project authorization (S3)** — fail-closed roots; “open this repo and code” path.  
2. **Gate dogfood (S7)** — enable for coding goals; strategist curriculum; measure.  
3. **Goal surface polish** — #61 sidebar + diff pane + role timeline; wire live votes end-to-end.  
4. **Plan mode profile + UX** — read-only grant + plan artifact + approve → build.  
5. **Mid-build resume design spike** — checkpoint service home in `coder-sandbox` / pack.

### Band B — Width safely (isolation already started)

6. **Specify merge-back** for multi-worktree workers.  
7. **Coding subagents (S6)** — explore read-only child; implementer child in worktree; Report + apply.  
8. **Expose `dispatch_parallel`** behind fan-out action when (6–7) exist.  
9. **Async delegate** for face agent research.  
10. **Optional parallel read tools** in executor.

### Band C — Series loops

11. Implement [`loops-plan.md`](../loops-plan.md) P1→P2→P3→P4.  
12. Ship `/loop` slash; dogfood one vault/doc or “keep CI green” series.

### Band D — Continuity & chrome

13. **Checkpoints / `/rewind` (S4)** — shadow-git or snapshot dir under data.  
14. **Headless coding CLI** with goal exit codes.  
15. **Repo orientation** — deepen `list_symbols` (#62); evaluate LSP later.  
16. **AGENTS.md / skills discovery** in coding perceive.  
17. **Coding permission modes** in TUI (cannot exceed capability ceiling).  
18. **Diff review UX** in TUI (supervise = see patch).

### Band E — Economics & correctness

19. **TE1** tool catalog narrowing diagnosis (spend).  
20. **#63/#64** cost + cache ordering (already counted present) operationalized in dogfood.

### Explicit non-goals (for this parity track)

- Plugin marketplace clone  
- Image/video tools  
- Matching OpenCode desktop/enterprise console  
- Replacing life-OS daemon with single-process `cd && agent` only (may *add* a thin headless path without abandoning daemon)

---

## 12. Suggested PR sequencing (for future implementers)

Single-responsibility slices that match existing Liberado plans:

| Seq | Slice | Plan refs | FOSS pressure |
|---|---|---|---|
| 1 | `[[projects]]` + fail-closed workspace | coding-tui S3 | Grok/OpenCode cwd-first |
| 2 | Plan profile + TUI mode | coding-tui / profiles | All three plan modes |
| 3 | Checkpoint + mid-build resume | S4, E6-c(b) | Grok/OpenCode |
| 4 | Coding explore subagent (read-only, no fan-out yet) | S6 start | All three explore agents |
| 5 | Worktree implementer subagent + merge | S6/C7 | Grok isolation |
| 6 | Fan-out action + `dispatch_parallel` wiring | agentic-loops concurrency | All three |
| 7 | Loops P1–P2 | loops-plan | Grok/Kimi |
| 8 | Gate default-on for coding + evals | S7 | Grok goal verify |
| 9 | TUI diff + permission modes + headless CLI | surface | Grok/OpenCode |
| 10 | Loops P3–P4 + dogfood series | loops-plan | Grok/Kimi |

---

## 13. Source index (claims → trees)

### Liberado

| Topic | Paths |
|---|---|
| Vocabulary / concurrency rule | `docs/spec/architecture/agentic-loops.md` |
| Loops design | `docs/future-work/loops-plan.md` |
| Coding TUI slices | `docs/future-work/coding-tui-plan.md`, `docs/roadmap.md` (Priority 3 — coding pack) |
| Goal hub / gate | `crates/session/` (`hub`, `completion_gate`, `event`) |
| Worktree | `crates/coder-sandbox/src/lib.rs` (`WorktreeWorkspace`) |
| Pack isolation wiring | `crates/coder-agent/src/session_pack/build.rs` |
| Tools | `crates/coder-tools/src/lib.rs` |
| Attempt loop / critic / gate adapter | `crates/coder-agent/` |
| Parallel API | `crates/orchestrator/` (`dispatch_parallel`) |
| Turn loop | `crates/executor/` |
| TUI / commands | `crates/tui/`, `crates/liberado-commands/` |
| Goals API | `crates/server/src/api/goals.rs` |
| Prior Grok-only note | `docs/future-work/ideas/vs-grok-build.md` (read-only) |

### Grok Build

| Topic | Paths |
|---|---|
| Goal loop | `grok-build/crates/codegen/xai-grok-shell/src/session/goal_*.rs`, `acp_session_impl/goal.rs` |
| Loop/scheduler | `…/tools/.../scheduler/`, user-guide `20-background-tasks.md` |
| Worktree | `xai-fast-worktree/`, `xai-grok-workspace/.../worktree/` |
| Subagents | `xai-tool-types/src/task.rs`, user-guide `16-subagents.md` |
| Plan / permissions | `plan_mode.rs`, `19-plan-mode.md`, `22-permissions-and-safety.md` |
| Checkpoints | `workspace/session/checkpoint.rs`, `17-sessions.md` |

### OpenCode

| Topic | Paths |
|---|---|
| Session loop | `opencode/packages/opencode/src/session/prompt.ts`, `packages/core/src/session/runner/` |
| Agents / plan | `packages/opencode/src/agent/agent.ts`, `tool/plan.ts`, docs `agents.mdx` |
| Worktree | `packages/opencode/src/worktree/`, experimental OpenAPI routes |
| Snapshot | `packages/opencode/src/snapshot/`, `session/revert.ts` |
| Permissions | `packages/opencode/src/permission/`, docs `permissions.mdx` |
| V2 architecture | root `CONTEXT.md`, `AGENTS.md` |

### Kimi Code

| Topic | Paths |
|---|---|
| Goals | `kimi-code/GOAL.md`, `docs/en/guides/goals.md`, `packages/agent-core-v2/src/agent/goal/` |
| Cron | `packages/agent-core/src/tools/cron/`, tools reference |
| Swarm / agents | `docs/en/customization/agents.md`, AgentSwarm tools |
| Plan / perms | plan service + `docs/en/guides/interaction.md` |
| Parallel tools | `packages/agent-core/src/loop/tool-scheduler.ts` |
| Surfaces | `apps/kimi-code/`, `apps/kimi-web/`, `packages/kap-server/` |

---

## 14. Bottom line

Liberado already has the **hard architecture** of an agentic coding harness — goal sessions, verifiers, capability topology, worktree isolation, and a coding tool pack — plus multi-surface life-OS strengths the FOSS trees do not try to own.

What stands between Liberado and **harness-level performance/parity** with Grok Build, OpenCode, and Kimi Code is mostly **productized width and continuity**, not a missing kernel:

1. Finish the **goal product** (gate dogfood, project auth, plan mode, mid-build resume, chrome).  
2. Turn worktree isolation into **safe parallel coding subagents** with merge-back.  
3. Build **`/loop` series memory** on top of existing cron + goals.  
4. Add **checkpoints/rewind**, richer repo tools, and interactive permission/diff UX.  
5. Fix **delegate seams and token economics** so multi-agent coding actually pays for itself.

Open PRs **#61–#64** (and draft **#2**) are counted as landed for this baseline; main already includes **#60** worktree isolation. The next roadmap should assume those surfaces exist and spend design effort on **loops, fan-out, resume, and plan/permission UX** — the remaining gaps that still separate Liberado from FOSS coding harnesses day-to-day.
