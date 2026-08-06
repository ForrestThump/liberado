# Liberado vs Grok Build / OpenCode / Kimi Code — Agentic Coding Harness Gap Analysis

**Status:** research (architect roadmap input) — **rebaselined on `develop` 2026-08-06**  
**Original cut:** 2026-08-05 (main + open-PRs-as-landed).  
**Current baseline:** branch **`develop`** tip `cf2bb90` (main + merged stack of project auth / plan / explore / dogfood write-up). Reliability fixes for self-host live on open PR **#71** (`fix/dogfood-findings-c2`), **proven by re-dogfood** session `01KZAM5M…` → PR **#70** succeeded unattended — treat #71 as *in-flight land*, not as inventing new capability.  
**Scope:** Agentic *coding* harness capabilities only — outer loops, isolation, tools, plan/verify, permissions, subagents, resume, coding surfaces. Not life-OS product market, billing, or multi-tenant SaaS.  
**Method:** In-tree sources for Liberado (`crates/*`, `docs/spec/architecture/*`, coding plans), three FOSS clones (`grok-build/`, `opencode/`, `kimi-code/`), and **live self-host dogfood** ([`self-host-coding-dogfood-2026-08.md`](../self-host-coding-dogfood-2026-08.md)).  
**Baseline rule (2026-08-06):** Judge Liberado from **what is on `develop` + proven dogfood**, not from “count every open GitHub PR as shipped.” Open PRs against *main* that were already merged into `develop` (#66–#68) count as shipped on this baseline. Draft #2 does not.  
**Non-edit note:** Older note [`ideas/vs-grok-build.md`](../ideas/vs-grok-build.md) overlaps Grok-only TUI framing; this document is the multi-harness, current-baseline deliverable and does not modify that file.

---

## 1. Executive summary (architect one-pager)

### What Liberado already is (coding) — `develop` 2026-08-06

Liberado is a **general agentic orchestration kernel** (goal sessions, budgets, terminals, capability zones, dual-store events, multi-surface clients) with a **first domain pack** for coding (`coder-*`). Coding is deliberately *not* the center of gravity of the product — but the coding pack is real, and **self-host is no longer theoretical**:

| Strength | Why it matters for coding performance | Develop evidence |
|---|---|---|
| **Goal-session kernel** | `/goal` surface, hub park/resume/cancel, SSE stream, named terminals — outer loop is a first-class object, not chat prose | Live: `POST /api/goals` domain `coding` end-to-end |
| **Maker ≠ checker** | Intake-frozen criteria, deterministic verifiers, critic on real git evidence, optional multi-reviewer **completion gate** (default **off**) | Gate still opt-in; dogfood used verifiers/progress path without gate on |
| **Capability / zone model** | Narrow-only delegation, risk-gated tools, proposals + approval ledger | Unchanged strength |
| **Project-root auth (S3)** | Fail-closed `[[projects]]`; `/goal in <name>`; inject `workspace_root` | **On develop** (#66 merge); `GET /api/projects` used in dogfood |
| **Plan + explore modes** | PathPolicy / CommandPolicy **presets** — plan writes only `.liberado/plan.md`; explore read-only catalog | **On develop** (#67 / #68); not full FOSS plan *approval UI* |
| **Worktree isolation** | Coding sessions on git repos get `WorktreeWorkspace` | **On develop** + Windows path fix (`ed8b910`); dogfood created real worktrees |
| **Git commit / push tools** | Branch → commit → push without shell `git` allow-list holes | **Proven live** (author `liberado@local`, PRs #69 / #70) |
| **Self-host dogfood (C2)** | Same harness editing liberado and opening a PR | **PR #69** (first run, rough); **PR #70** (re-dogfood after #71, clean succeed) |
| **Durable multi-surface** | Daemon + TUI + HTTP/SSE + Telegram | Unchanged |
| **Coding tool core** | Files, search, patch, git, shell, validate, `list_symbols` | On main/`develop` |

### Where Liberado is still behind purpose-built coding harnesses

| Gap class | Liberado on `develop` (2026-08-06) | FOSS reference |
|---|---|---|
| **Time-based series (`/loop`)** | Designed (`loops-plan.md`); **still zero production code** | Grok `/loop` + scheduler; Kimi session cron; OpenCode external |
| **Agent-reachable parallelism** | `dispatch_parallel` **built but unwired**; tools **serial**; `delegate` **sync await** | Grok/Kimi/OpenCode multi-subagent + parallel tool settle |
| **Coding subagent productization** | Explore/plan are **payload modes on one coding session**, not typed child agents with worktree merge-back | Grok `task` + isolation; OpenCode `task` + children; Kimi Agent/Swarm |
| **Plan mode as product UX** | **Policy preset shipped** (`/plan`, plan artifact only) — **no** exclusive approval UI / mid-session plan→build grant widen | Grok/OpenCode/Kimi plan FSM + approval chrome |
| **Workspace rewind / checkpoints** | Park session yes; **mid-build resume false**; no shadow-git `/rewind` | Grok FS checkpoints; OpenCode snapshot/revert |
| **Outer-loop reliability under load** | First self-host worked only after several hard bugs (see dogfood write-up); #71 hardens the path; **gate still default-off**; intake still fragile without schema | Grok/Kimi productized bail/pause taxonomy + headless exit codes |
| **Interactive coding chrome** | Goal sidebar + stream; still thin on diff pane, permission modes, task dashboard, headless `-p` | Grok/OpenCode/Kimi product UIs |
| **Repo intelligence** | `list_symbols` + grep — no graph/LSP product | Grok graph+LSP; OpenCode LSP/grep/glob |
| **Skills / plugins / hooks market** | Topology, MCP, prompts — not agent skill marketplace | All three FOSS ecosystems stronger |

### Where Liberado is ahead (do not regress)

- **Domain-agnostic goal kernel** (life + coding share hub vocabulary).
- **Deterministic verifier pipeline** as success authority (OpenCode core lacks this; Grok/Kimi verify more via model panels / culture).
- **Life-OS composition**: cron daemon, Telegram, vault, multi-MCP mesh, PR-dispatch farms — not coding-only.
- **Fail-closed capability topology** (pools, profiles, grants) rather than only session permission modes.

### What “perform as well as these harnesses” means for Liberado

Not “clone Grok Build’s TUI.” It means, for **coding jobs of comparable difficulty**:

1. **Outer success loop reliability** — keep working until evidence-backed terminal, with stall recovery and human-visible state. **Partially proven** by self-host PR dogfood; still short of Grok/Kimi bail taxonomy, gate default-on, mid-build resume.
2. **Safe width** — independent exploration/edits in **isolated workspaces**, then merge. Isolation primitive **shipped**; fan-out product **still missing**.
3. **Interactive control plane** — plan → approve → implement; permission modes; rewind. Plan *policy* **shipped**; approval UI + rewind **still missing**.
4. **Recurring improvement** — series memory over goals (`/loop`). **Still unbuilt.**
5. **Repo orientation speed** — structural overview. `list_symbols` only; still thrash-prone vs graph/LSP.

### Roadmap shape (recommended bands) — after develop rebaseline

| Band | Theme | Status after develop |
|---|---|---|
| **A — Finish goal product** | Gate dogfood, project auth, plan mode, goal chrome, mid-build resume | **Auth + plan/explore presets landed.** Gate still off. Chrome + resume still open. Self-host reliability: #71. |
| **B — Isolation → width** | Parallel tools / coding subagents / merge-back | Isolation yes; **width still the major gap** |
| **C — Series loops** | `loops-plan.md` P1–P4 | **Unchanged major gap** |
| **D — Coding chrome + tools** | Checkpoints, approval UI, headless CLI, LSP depth | **Unchanged** |
| **E — Measure** | Gate/strategist evals, curriculum, TE1 | Dogfood numbers still thin; cost tools exist |

---

## 2. Liberado baseline — `develop` branch (2026-08-06)

### 2.1 What `develop` is

Integration branch cut from `main` (`5a52043` / PR #65 gap analysis merge), then stacked with coding-harness work that was open against main:

| Commit / PR | Capability on `develop` |
|---|---|
| main through **#65** | Worktree isolation (#60), goal sidebar (#61), `list_symbols` (#62), cost subcommands (#63), face-agent cache-order test (#64), this analysis (#65) |
| **#66** merge | Project-root authorization — `[[projects]]`, `authorize_coding_workspace`, `GET /api/projects`, coding `POST /api/goals` inject |
| **#67** merge | Plan mode — `PathPolicy::plan_mode()`, `CommandPolicy::none_allowed()`, `/plan`, payload `plan_mode` |
| **#68** merge | Explore mode — `PathPolicy::read_only()`, catalog filter, `/explore`, payload `explore_mode` |
| **`ed8b910`** | Windows worktree path fix (strip `\\?\`; unique worktree id) — found by first self-host dogfood |
| **`cf2bb90`** | Dogfood findings write-up ([`self-host-coding-dogfood-2026-08.md`](../self-host-coding-dogfood-2026-08.md)) |

**Not on `develop` tip yet (open PR #71 onto develop):** no-changes-after-commit gate, live tool stream events, intake flexible decode + project context, worktrees under data dir, `gh pr create --base` preflight. **Re-dogfood on that branch succeeded unattended** (session `01KZAM5M…` → PR #70). Merge #71 before declaring self-host “production reliable.”

**Explicitly excluded:** draft PR **#2** (auto agent WebUI HTML edit) — never merged into develop.

**GitHub still shows #66–#68 open against `main`:** they are **merged into develop**, not necessarily into main. For harness status, **`develop` is truth.**

### 2.2 Live performance evidence (dogfood)

| Run | Session | Outcome | What it proved |
|---|---|---|---|
| First self-host | `01KZAJN9…` | Branch + commit + push + PR #69; **failed validation after commit**; human retry for `gh pr create`; first PR base wrong until `develop` existed on remote | Tools + worktree + push work; **progress gate punished clean tree after commit**; stream had almost no tool events; intake/DeepSeek broken; Windows worktree paths |
| After path fix only | (failed early) | Worktree path blocker | Fixed on develop (`ed8b910`) |
| Re-dogfood on **#71** binary | `01KZAM5M…` | **`succeeded`** unattended; 16 live tool events; file_changed; validation ok; PR #70 base `develop` | P0 no-changes-after-commit + live tools + gh base preflight hold under real load |

Write-up: [`self-host-coding-dogfood-2026-08.md`](../self-host-coding-dogfood-2026-08.md).

### 2.3 How this changes the original cut

| Original claim (2026-08-05) | Develop rebaseline |
|---|---|
| Project auth open (G-A3) | **Closed on develop** |
| Plan mode “not first-class” / Band A open | **Preset shipped** (#67); FOSS-style approval UI still open |
| Explore agents only via FOSS | **Explore mode preset shipped** (#68) — same session, not a child agent |
| C2 dogfood not run | **Run twice**; self-host is real |
| Open-PR baseline (#61–#64) forward-looking | Those PRs **are on main**; develop is ahead of main for coding modes + auth |
| Worktree “on par” | Still true **as primitive**; dogfood found Windows + id bugs (fixed) |

### Doc drift warning

Prefer **`develop` code + dogfood write-up** over older roadmap rows that still say project auth / plan mode / dogfood are open. Main lags develop until #66–#68 (and #71) land there.
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
| Success-driven outer loop (`/goal`) | **Y** (kernel + slash + API; gate opt-in; **self-host dogfood proven**) | **Y** (planner → worker → adversarial verify) | **N** core (plugin) | **Y** (driver + tools + queue) |
| Frozen acceptance criteria | **Y** (intake / `GoalContract` / verifiers; intake still fragile without json_schema — skip for scripted runs) | **Y** (contract culture + plan file) | **P** (plan/todos informal) | **P** (proof-of-done culture + tools) |
| Multi-attempt repair | **Y** (`coder-agent` attempts + feedback; #71: commits count as progress) | **Y** | **P** (steps budget) | **Y** (continuation while active) |
| Independent completion verification | **Y** (verifiers + critic + optional gate) | **Y** (skeptic panel, fail-closed) | **N** harness-level | **P** (model re-judge; bash tests) |
| Goal pause / resume / status UX | **Y** (`/goal` + park/resume API; sidebar #61) | **Y** | **—** | **Y** |
| Goal queue | **N** | **P** | **N** | **Y** (`/goal next`) |
| Time-based `/loop` or series | **P** (design only — **still the largest design-ready gap**) | **Y** (`/loop`, scheduler_*) | **N** (external) | **Y** (session cron tools) |
| System cron (daemon) | **Y** (life-OS strength) | **P** (session scheduler) | **P** (GitHub Action) | **P** (session-bound) |
| Self-host PR path (commit→push→PR) | **P→Y** (tools yes; dogfood PRs #69/#70; reliability #71) | **Y** | **Y** | **Y** |

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
| Plan mode FSM + exclusive plan file | **P→Y-lite** (**/plan** + PathPolicy plan artifact only on develop; no approval UI / plan→build grant widen) | **Y** | **Y** (build/plan agents) | **Y** |
| Explore / read-only coding mode | **Y-lite** (**/explore** + read_only PathPolicy + catalog filter on develop) | **Y** (explore subagent) | **Y** (explore agent) | **P** |
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

### 5.1 Liberado (baseline = `develop` + dogfood)

**Architecture** ([`agentic-loops.md`](../../spec/architecture/agentic-loops.md)):

```
turn loop (executor) ⊂ goal (session hub + pack) ⊂ loop (planned) ⊂ meta-loop (tuner)
```

| Layer | Status | Evidence |
|---|---|---|
| Hub lifecycle | Shipped | `crates/session` — start, park, resume, cancel, stream, grants |
| Coding attempt loop | Shipped | `crates/coder-agent` — intake → (plan) → worker → verifiers → critic/gate → repair |
| Slash UX | Shipped | `/goal`, `/goal in <project>`, **`/plan`**, **`/explore`**, status/pause/resume/clear |
| Completion gate | Shipped, **default off** | Still the biggest *optional* quality lever vs Grok |
| Surface chrome | Partial | Sidebar + live gate votes; tool events live only with #71; role/verifier widgets still thin |
| Project authorization | **Shipped on develop** | `[[projects]]` + fail-closed + `GET /api/projects` (#66) |
| Diff API | Shipped | `GET /api/goals/{id}/diff` |
| Self-host dogfood | **Proven** | PRs #69 (rough), #70 (clean on #71 binary) |

**Performance posture vs FOSS (after dogfood):** Liberado’s **philosophy is stronger than OpenCode** (criteria + verifiers + maker≠checker). Vs **Grok Build**, Liberado has the same *shape* (disputed completion, strategist) but:

- Gate remains **opt-in** (cost: `1 + fresh_reviewers` model calls per attempt) — dogfood ran with gate off.
- First self-host exposed **harness bugs unit tests never saw** (Windows worktrees, no-changes-after-commit, silent tool stream, intake schema). Those are the difference between “architecture exists” and “harness performs.”
- Grok still wins on **premature-stop detectors**, **laziness classifiers**, richer pause taxonomy, coding-first TUI.
- Kimi still wins on **goal queue**, **hard budgets with headless exit codes**, main-only goal tools.

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

| # | Gap | Status 2026-08-06 | What needs to happen |
|---|---|---|---|
| G-A1 | Gate default-off | **Still open** | Dogfood S7: measure cost/quality; enable for coding profiles / “strict” goals; keep opt-out |
| G-A2 | Live goal stream incomplete | **Partial** — sidebar landed; tool events on #71; role/verifier panes still thin | Finish panes; ensure gate votes stream when gate on |
| G-A3 | Project root auth | **Done on develop** (#66) | Keep operator `[[projects]]` hygiene; TUI picker polish optional |
| G-A4 | Mid-build resume | **Still open** | Checkpoint + `can_resume` after coder role (E6-c(b)) |
| G-A5 | Goal queue | **Still open** | Optional Kimi-style queue on hub |
| G-A6 | Premature bail | **Still open** | Stop-phrase / no-progress classifiers at goal layer (Grok) |
| G-A7 | Headless goal CLI | **Still open** | `liberado goal -p` with exit codes for CI/self-improvement |
| G-A8 | Self-host reliability | **Mostly closed by dogfood + #71** | Merge #71; keep re-dogfood as regression bar |

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

**Shipped (main #60 + develop hardening):**

- `WorktreeWorkspace` in `crates/coder-sandbox` — `git worktree add`, checkout HEAD, Drop cleanup, path containment tests.
- Coding pack `session_pack/build.rs`: **Worktree if workspace is a git repo, else HostLocal**.
- Tools/verifiers operate on effective worktree root.
- **Dogfood:** real worktrees under self-host; Windows extended-path strip on develop (`ed8b910`); #71 moves worktrees under data dir + unique session ids.

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

### 8.2 Plan mode (and explore)

| System | Model |
|---|---|
| Liberado **develop** | **Shipped presets:** `/plan` → `PathPolicy::plan_mode()` (only `.liberado/plan.md`) + no shell; `/explore` → read_only + catalog filter. **Missing:** approval UI, mid-session plan→build grant widen, typed explore *child* agent |
| Grok | Plan FSM; only plan.md writable; approval UI with line comments |
| OpenCode | Primary `plan` agent vs `build`; Tab switch; plan_exit confirmation |
| Kimi | Enter/ExitPlanMode; write sandbox; review card |

**What needs to happen now:** Not another permission system — **UX on top of the presets**: TUI mode indicator, plan review/approve, then a normal `/goal` for build (or explicit widen). Optional: explore as a *spawned* coding child when S6 lands.

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

These are not “missing features”; they are **performance and reliability debt** that FOSS harnesses either avoid or productized around. **Updated from live dogfood.**

1. **Gate cost vs quality** — multi-reviewer gate still **off** by default; dogfood never measured it on.
2. **Serial tools** — multi-tool model turns wait in line; latency vs OpenCode/Kimi parallel settle.
3. **Sync delegate** — multi-hop coding research cannot overlap; face turn blocked.
4. **Token economics** — orchestrator ~11k base context re-sent (TE1); face protection not paying off in measurements.
5. **Worktree without apply UX** — isolation without merge productization; parallel workers (when added) still lack a user story. Dogfood used single-session worktree successfully.
6. **Can_resume false after build starts** — long coding jobs are not restart-safe mid-diff.
7. **No series memory on cron** — until loops land.
8. **Intake + non-schema providers** — DeepSeek rejects `json_schema`; unconstrained JSON breaks without flexible decode (#71) or `intake.enabled=false`.
9. **Progress gate vs git_commit** — *was* a hard self-host bug (clean tree after commit = NoChanges); fixed on #71. Keep regression tests.
10. **Observability** — tool events were invisible on the goal stream until #71 LIVE_GATE mirror; still thin vs FOSS dashboards.
11. **Doc/branch drift** — main lags develop; prefer develop + this §2 over “open on GitHub main” as truth.
12. **VTCode/PR-dispatch path** — external factory; keep coding pack path pure (dogfood used pack tools, not vtcode).

---

## 10. Feature parity scorecard (honest) — rebaselined

Relative to **coding harness job** (not life-OS), **`develop` + dogfood evidence**:

| Area | Liberado vs best of FOSS | Verdict 2026-08-06 |
|---|---|---|
| Goal kernel / criteria / verifiers | Competitive or **ahead** (esp. vs OpenCode) | **Parity-capable**; enable gate dogfood next |
| Self-host PR loop (edit→commit→push→PR) | FOSS do this daily | **Parity-capable** after #71; was broken by harness bugs, not missing tools |
| Goal UX / queue / bail guards | Behind Grok/Kimi | **Gap** (unchanged) |
| `/loop` series | Behind Grok/Kimi; design ready | **Major gap** (unchanged) |
| Worktree isolation primitive | On par with OpenCode; behind Grok polish | **Parity** (primitive); Windows hardening landed |
| Parallel dispatch product | Behind all three | **Major gap** (unchanged) |
| Plan / explore **modes** | Presets on develop; behind FOSS approval UX | **Partial** (moved up from full gap) |
| Interactive permission UX | Behind all three | **Gap** |
| Coding tool depth (LSP/graph/web) | Behind Grok/OpenCode | **Gap** |
| Checkpoints/rewind | Behind Grok/OpenCode | **Major gap** |
| Multi-surface coding apps | Behind OpenCode/Kimi | Accept or W1 later |
| Skills/plugins marketplace | Behind all three | **Medium** |
| Capability/trust topology | **Ahead** | Preserve |
| Project-root auth | Competitive (fail-closed allowlist) | **Parity** on develop |
| Daemon / multi-domain / Telegram | **Ahead** | Preserve |

---

## 11. Architect roadmap implications (ordered) — after develop rebaseline

Priority is for **coding harness performance + parity**, assuming life-OS P1 continues in parallel.

### Band A — Goal product completion

| # | Item | Develop status |
|---|---|---|
| 1 | Project authorization (S3) | **Done** on develop (#66) |
| 2 | Plan / explore **policy presets** | **Done** on develop (#67 / #68) |
| 3 | Self-host reliability (dogfood findings) | **Done on #71** — merge to develop next |
| 4 | Gate dogfood (S7) | **Still open** — highest remaining A-band quality lever |
| 5 | Goal surface polish | Partial (sidebar yes; tool stream on #71; panes/diff UX open) |
| 6 | Plan **approval** UX + plan→build handoff | **Still open** (preset ≠ product chrome) |
| 7 | Mid-build resume | **Still open** (E6-c(b)) |

### Band B — Width safely (isolation already started)

Unchanged major gap: isolation without fan-out.

8. Specify merge-back for multi-worktree workers.  
9. Coding subagents (S6) — explore as *child* (not only payload mode); implementer worktree + Report + apply.  
10. Expose `dispatch_parallel` when (8–9) exist.  
11. Async delegate for face research.  
12. Optional parallel **read** tools in executor.

### Band C — Series loops

Still the largest *design-ready, code-zero* gap.

13. Implement [`loops-plan.md`](../loops-plan.md) P1→P2→P3→P4.  
14. Ship `/loop` slash; dogfood one vault/doc or “keep CI green” series.

### Band D — Continuity & chrome

15. Checkpoints / `/rewind` (S4).  
16. Headless coding CLI with goal exit codes.  
17. Repo orientation depth beyond `list_symbols`.  
18. AGENTS.md / skills discovery.  
19. Coding permission modes in TUI (cannot exceed capability ceiling).  
20. Diff review UX.

### Band E — Economics & correctness

21. TE1 catalog narrowing.  
22. Attach `liberado-cost --json` to every dogfood write-up.  
23. Keep self-host re-dogfood as a **regression bar** (not a one-off).

### Explicit non-goals (for this parity track)

- Plugin marketplace clone  
- Image/video tools  
- Matching OpenCode desktop/enterprise console  
- Replacing life-OS daemon with single-process `cd && agent` only (may *add* a thin headless path without abandoning daemon)

---

## 12. Suggested PR sequencing (for future implementers)

| Seq | Slice | Status | FOSS pressure |
|---|---|---|---|
| 1 | `[[projects]]` + fail-closed workspace | **Done on develop** | — |
| 2 | Plan + explore PathPolicy presets | **Done on develop** | — |
| 3 | Dogfood reliability (#71) | **Open PR; proven** | Self-host bar |
| 4 | Merge #71 → develop → main stack | Ops | — |
| 5 | Gate default-on + measure | Open | Grok goal verify |
| 6 | Checkpoint + mid-build resume | Open | Grok/OpenCode |
| 7 | Plan approval UX / mode chrome | Open | All three |
| 8 | Coding explore *child* + implementer worktree + merge | Open | All three |
| 9 | Fan-out + `dispatch_parallel` | Open | All three |
| 10 | Loops P1–P2 | Open | Grok/Kimi |
| 11 | TUI diff + permission modes + headless CLI | Open | Grok/OpenCode |
| 12 | Loops P3–P4 + series dogfood | Open | Grok/Kimi |

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
| Project auth | `crates/config-loader` (`authorize_coding_workspace`), `goals.rs` |
| Plan / explore policies | `crates/coder-agent/src/session_pack/policies.rs`, `coder-core` PathPolicy |
| Self-host dogfood | `docs/future-work/self-host-coding-dogfood-2026-08.md` |
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

## 14. Bottom line (rebaselined 2026-08-06)

### What changed since the original cut

On **`develop`**, Liberado closed the “can we even point the coder at a real repo and plan/explore safely?” cluster:

- Project auth, plan mode, explore mode, worktree isolation, git tools, and **a real self-host PR**.
- Dogfood then proved that **architecture ≠ performance**: the first run fell over on Windows worktrees, post-commit “no changes,” silent tool streams, and intake schema — not on missing `git_commit`.

After reliability fixes (**#71**, proven by PR **#70**), Liberado is **parity-capable on the single-session self-host loop** that C2 asked for. That is a real step up from the 2026-08-05 forward-looking baseline.

### What still separates Liberado from FOSS coding harnesses day-to-day

Ordered by how much they hurt *performance and feel* vs Grok/OpenCode/Kimi:

| Rank | Gap | Why it still matters |
|---|---|---|
| **1** | **Parallel width** (fan-out + coding subagents + merge-back) | Isolation exists; nothing can use it concurrently — FOSS daily driver path |
| **2** | **`/loop` series memory** | Design ready, zero code — FOSS recurring improvement |
| **3** | **Checkpoints / mid-build resume** | Long jobs are not restart-safe; FOSS recover |
| **4** | **Gate default-on + bail taxonomy** | Maker≠checker is optional in practice until dogfooded on |
| **5** | **Plan/permission/diff chrome** | Presets exist; product UX does not match FOSS cadence |
| **6** | **Repo intelligence + headless CLI** | Orientation thrash and CI self-improvement lag |

### Preserve

- Domain-agnostic goal kernel, deterministic verifiers, capability topology, multi-surface life-OS, daemon durability.

### Immediate next moves

1. **Merge #71** into develop (and eventually main) so the reliability bar is the default tip.  
2. **Gate measure / optional default-on** for coding profiles (S7).  
3. **Pick one of:** mid-build resume, plan-approval chrome, or coding-subagent merge-back — do not open fan-out before merge-back is specified.  
4. **Loops P1** when recurring improvement becomes the bottleneck (it is not yet — width and continuity are).  
5. Keep **self-host re-dogfood** as a regression test, not a one-off story.

**Baseline truth:** branch **`develop`**, dogfood write-up, and PR #71 proof — not “open on GitHub main” and not draft #2.