# Liberado vs Grok Build / OpenCode / Kimi Code — Agentic Coding Harness Gap Analysis

**Status:** research (architect roadmap input) — **updated on `develop` 2026-08-06 (post-#76)**  
**Original cut:** 2026-08-05 (main + open-PRs-as-landed).  
**Prior rebaseline:** 2026-08-06 afternoon (`4080608` + open #73).  
**Current baseline:** branch **`develop`** tip **`4fb57e1`** (includes #66–#76, dogfood reliability **#71**, self-host dogfood **#70**, coding fan-out **#72**, checkpoints + mid-build resume + rewind **#73**, generic ship preflight gate **#74**, coverage gap analysis + mutant tests **#75**, configurable hashline edit mode **#76**). Draft **#2** closed path only; dogfood artifact **#69** closed without merge (superseded).  
**Scope:** Agentic *coding* harness capabilities only — outer loops, isolation, tools, plan/verify, permissions, subagents, resume, coding surfaces. Not life-OS product market, billing, or multi-tenant SaaS.  
**Method:** In-tree sources for Liberado (`crates/*`, `docs/spec/architecture/*`, coding plans), three FOSS clones (`grok-build/`, `opencode/`, `kimi-code/`), and **live self-host dogfood** ([`self-host-coding-dogfood-2026-08.md`](../self-host-coding-dogfood-2026-08.md)).  
**Baseline rule:** Judge Liberado from **what is on `develop` + proven dogfood**, not from “open on GitHub main.”  
**Non-edit note:** Older note [`ideas/vs-grok-build.md`](../ideas/vs-grok-build.md) overlaps Grok-only TUI framing; this document is the multi-harness, current-baseline deliverable and does not modify that file.

---

## 1. Executive summary (architect one-pager)

### What Liberado already is (coding) — `develop` tip `4fb57e1`

Liberado is a **general agentic orchestration kernel** with a **first domain pack** for coding (`coder-*`). Self-host, **multi-worktree fan-out**, and **mid-build checkpoint/resume** are no longer theoretical:

| Strength | Why it matters | Develop evidence |
|---|---|---|
| **Goal-session kernel** | Outer loop is a first-class object | Live `POST /api/goals` domain `coding` |
| **Maker ≠ checker** | Intake, verifiers, critic, optional multi-reviewer gate | Gate still **default off** |
| **Project-root auth (S3)** | Fail-closed real repos | #66 on develop; dogfood used `[[projects]]` |
| **Plan + explore modes** | PathPolicy presets (`/plan`, `/explore`) | #67 / #68 on develop |
| **Worktree isolation** | Per-session isolation before fan-out | #60 + Windows path fix + data-dir worktrees (#71) |
| **Self-host PR path (C2)** | Edit → commit → push → PR without TUI | #71 reliability + #70 clean re-dogfood (#69 closed as artifact) |
| **Coding fan-out (S6 v1)** | Parallel worktree **hub children** + parent LLM merge-back | **#72 on develop** — `payload.subtasks`, concurrency default **3**, children never self-merge |
| **Checkpoints + mid-build resume (S4)** | Shadow-git checkpoints per attempt + per write-flush; durable park/resume + rewind | **#73 on develop** — `ShadowGit`, `can_resume` after coder role, `POST …/rewind` |
| **Ship preflight gate** | Generic `PreflightRunner` + coding pack hooks; CI-equivalent ship bar before `Succeeded` | **#74 on develop** — project-configurable preflight steps |
| **Git tools** | branch/commit/push | Proven live (`liberado@local`) |
| **Durable multi-surface** | Daemon + TUI + HTTP/SSE + Telegram | Unchanged |
| **Coding tool core** | FS, search, patch, git, shell, `list_symbols`, **hashline edit** | On develop; hashline edit mode (#76) |

### Where Liberado is still behind purpose-built coding harnesses

| Gap class | Liberado on `develop` (post-#76) | FOSS reference |
|---|---|---|
| **Time-based series (`/loop`)** | Design only — **still zero production code** | Grok `/loop`; Kimi session cron |
| **Within-turn tool parallelism** | Executor tools still **serial** | OpenCode/Kimi parallel settle |
| **Face `delegate` → coding** | Face still routes to **dispatch** domain only; coding fan-out is pack `subtasks`, not chat delegate | Grok/OpenCode/Kimi task tools from chat |
| **Coding subagent productization** | **S6 v1 landed** (hub children + merge); missing: face entry, parent completion gate over merge, TUI multi-child chrome, nested fan-out | Grok/OpenCode/Kimi product UX |
| **Plan mode as product UX** | Preset shipped; **no** approval UI / plan→build grant widen | All three FOSS |
| **Workspace rewind / checkpoints** | **S4 landed (#73):** shadow-git checkpoints + park/resume + rewind | Grok/OpenCode |
| **Gate default-on + bail taxonomy** | Gate opt-in; no Grok-style pause classifiers | Grok/Kimi |
| **Interactive coding chrome** | Sidebar + stream; thin diffs/permission modes/dashboard | Grok/OpenCode/Kimi product UIs |
| **Repo intelligence** | `list_symbols` + grep + **hashline edit (#76)** | Grok graph+LSP; OpenCode LSP/glob |
| **Headless CLI packaging** | Daemon+API headless **proven**; no `liberado goal -p` exit codes | `grok -p` / `kimi -p` |
| **Skills / plugins market** | MCP topology, not skill marketplace | All three |

### Where Liberado is ahead (do not regress)

- **Domain-agnostic goal kernel** (life + coding share hub vocabulary).
- **Deterministic verifier pipeline** as success authority (OpenCode core lacks this; Grok/Kimi verify more via model panels / culture).
- **Life-OS composition**: cron daemon, Telegram, vault, multi-MCP mesh, PR-dispatch farms — not coding-only.
- **Fail-closed capability topology** (pools, profiles, grants) rather than only session permission modes.

### What “perform as well as these harnesses” means for Liberado

Not “clone Grok Build’s TUI.” It means, for **coding jobs of comparable difficulty**:

1. **Outer success loop reliability** — **Self-host PR path proven** (#70/#71); still short of Grok/Kimi bail taxonomy, gate default-on. **Mid-build resume landed (#73).**
2. **Safe width** — Isolation + **hub fan-out + LLM merge (#72)** landed. Still missing face-entry, serial tools, classifier fan-out, product chrome.
3. **Interactive control plane** — Plan *policy* shipped; **rewind landed (#73)**; approval UI still missing.
4. **Recurring improvement** — `/loop` **still unbuilt** (now the largest design-ready zero-code gap).
5. **Repo orientation / CLI packaging** — efficiency and CI ergonomics, not blockers for single PRs.

### Roadmap shape (recommended bands) — post-#76

| Band | Theme | Status |
|---|---|---|
| **A — Goal product** | Auth, plan/explore, self-host reliability | **Mostly done.** Gate dogfood + chrome remain. Mid-build resume landed (#73). Preflight gate landed (#74). |
| **B — Isolation → width** | Subagents + merge-back | **S6 v1 done (#72).** Face entry, tool parallel, polish remain |
| **C — Series loops** | `loops-plan.md` | **Now the top major zero-code gap** |
| **D — Continuity & chrome** | Checkpoints, UX, headless CLI, LSP | **S4 done (#73):** shadow-git checkpoints + rewind. Plan approval UX, headless CLI, LSP remain. |
| **E — Measure** | Gate evals, cost on dogfood | Still thin |

---

## 2. Liberado baseline — `develop` tip `4fb57e1` (2026-08-06 evening)

### 2.1 What `develop` is now

Integration branch cut from `main` (PR #65 gap analysis), then:

| Landed | Capability |
|---|---|
| main → **#65** | Worktree (#60), goal sidebar (#61), `list_symbols` (#62), cost (#63), face cache-order (#64), this analysis |
| **#66** | Project-root authorization |
| **#67** / **#68** | Plan + explore PathPolicy presets |
| **#71** (+ path fix) | Self-host reliability: no-changes-after-commit, live tool events, intake flexible decode, data-dir worktrees, `gh pr create --base` preflight |
| **#70** | Clean re-dogfood PR (docs note) |
| **#72** | **S6 fan-out:** hub-spawned coding children on worktree branches, parent LLM merge-back, `max_concurrent_coding_subagents` **default 3** |
| **#73** | **S4 checkpoints + mid-build resume + rewind:** shadow-git checkpoints per attempt + per write-flush; durable worktree park/resume; `can_resume` after coder role; `POST …/rewind` |
| **#74** | **Generic ship preflight gate:** `PreflightRunner` + coding pack hooks; CI-equivalent full matrix blocks `Succeeded`; project-configurable steps |
| **#75** | **Coverage gap analysis + mutant tests:** mutants covered in coder-sandbox, coder-core, config-loader; clippy fixes for Rust 1.94; ~50 tests added |
| **#76** | **Hashline edit mode:** configurable hashline edit mode for the coding harness |

**Closed without merge:** **#69** (first dogfood one-liner; superseded by write-up + #70/#71). Draft **#2** still not in develop.

**Main vs develop:** coding stack above is on **`develop`**; main may lag until those PRs land there. Harness truth = develop.

### 2.2 Live performance evidence (dogfood)

| Run | Session | Outcome | What it proved |
|---|---|---|---|
| First self-host | `01KZAJN9…` | Branch + commit + push + PR #69; **failed validation after commit**; human retry for `gh pr create`; first PR base wrong until `develop` existed on remote | Tools + worktree + push work; **progress gate punished clean tree after commit**; stream had almost no tool events; intake/DeepSeek broken; Windows worktree paths |
| After path fix only | (failed early) | Worktree path blocker | Fixed on develop (`ed8b910`) |
| Re-dogfood on **#71** binary | `01KZAM5M…` | **`succeeded`** unattended; 16 live tool events; file_changed; validation ok; PR #70 base `develop` | P0 no-changes-after-commit + live tools + gh base preflight hold under real load |
| Fan-out unit/hub tests (**#72**) | `fanout_*` tests | Clean dual-worktree merge; conflict + LLM resolve; **hub-spawned session ids** | Parallel width + merge protocol answered |

Write-up: [`self-host-coding-dogfood-2026-08.md`](../self-host-coding-dogfood-2026-08.md). Fan-out: `crates/coder-agent/src/fanout.rs`, `coder-sandbox/src/merge.rs`.

### 2.3 How this changes the original cut

| Original claim (2026-08-05) | Develop now (`4fb57e1`) |
|---|---|
| Project auth open (G-A3) | **Closed** (#66) |
| Plan mode not first-class | **Preset shipped** (#67); FOSS approval UI still open |
| Explore only via FOSS | **Explore preset shipped** (#68) |
| C2 dogfood not run | **Run twice**; #69 closed as artifact; #70 clean |
| Self-host reliability open | **Closed** (#71 on develop) |
| Coding subagents / merge-back open (G-W2/G-W5) | **S6 v1 closed** (#72): hub children + parent LLM merge; concurrency default **3** |
| Parallel width "major gap" | **Partial** — coding fan-out exists; tool serial + face delegate still open |
| Open-PR baseline forward-looking | Prefer develop tip over main-open list |
| Checkpoints / mid-build resume open | **Closed** (#73): shadow-git snapshots + park/resume/rewind; `can_resume` after coder role |
| Completion gate only single-reviewer | **Still opt-in**; preflight gate (#74) adds CI-equivalent ship bar before terminal success |
| No hashline edit anchoring | **Closed** (#76): configurable hashline edit mode in coding harness |

### Doc drift warning

Prefer **`develop` tip + this file + dogfood write-up**. Main lags until the stack lands there.

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
| Parallel subagent fan-out | **P→Y-lite** (**#72** coding `payload.subtasks` hub children, max 3; MCP `dispatch_parallel` still unwired; face `delegate` still dispatch-only) | **Y** (task + wait_tasks + workflows) | **Y** (task / background experimental) | **Y** (Agent + AgentSwarm) |
| Parallel tool calls (one turn) | **N** (serial `for` loop) | **P** (flagged parallel dispatch) | **Y** (eager settle) | **Y** (resource-aware scheduler) |
| Merge-back story | **Y-lite** (#72 parent sequential merge + LLM conflicts; no TUI apply chrome) | **Y** (apply into main) | **P** | **—** |

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
| FS checkpoints / rewind | **Y** (shadow-git #73: snapshots per attempt + write-flush; restore + rewind) | **Y** | **Y** (snapshot/revert) | **N** (conversation undo) |
| Mid-build coding resume | **Y** (#73: `can_resume` after coder role; durable worktree park/resume) | **Y** | **Y** (session continue) | **Y** (session; goal → paused) |
| Headless one-shot coding CLI | **P** (API/evals/PR factory) | **Y** (`grok -p`) | **Y** | **Y** (`kimi -p`, goal exit codes) |
| ACP / editor protocol | **N** planned | **Y** | **Y** | **Y** |
| Hashline edit anchoring | **Y** (#76: configurable hashline edit mode) | **Y** (hashline anchors) | **P** | **—** |

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
| G-A4 | Mid-build resume | **Done on develop** (#73) | Shadow-git checkpoints + `can_resume` + durable worktree park/resume landed; keep re-dogfood |
| G-A5 | Goal queue | **Still open** | Optional Kimi-style queue on hub |
| G-A6 | Premature bail | **Still open** | Stop-phrase / no-progress classifiers at goal layer (Grok) |
| G-A7 | Headless goal CLI | **Still open** | `liberado goal -p` with exit codes for CI/self-improvement |
| G-A8 | Self-host reliability | **Closed on develop (#71)** | Keep re-dogfood as regression bar |

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

**Shipped (main #60 + develop through #72):**

- `WorktreeWorkspace` + Windows path strip + data-dir worktrees.
- Coding pack: Worktree for normal builds; **HostLocal inside fan-out child worktrees**.
- **#72 S6:** `payload.subtasks` → parallel hub coding sessions (`start_background` / `await_terminal`), grant without AskHuman, named branches `fanout/<label>-i`, parent-only sequential merge + LLM conflict resolve (`fanout.rs`, `merge.rs`). Default concurrency **3** (`max_concurrent_coding_subagents`). Nested subtasks refused.
- Hub race fix: durable `finish()` before `SessionFinished` so awaiters see results.

**Still open:**

| Piece | Status |
|---|---|
| Face `delegate` → coding | Still domain **dispatch** only; coding fan-out is pack payload, not chat tool |
| MCP `dispatch_parallel` | Built; still unwired (correctly not used for coding merge) |
| Parallel tools in executor | Still serial |
| Parent completion gate over merged tree | Fan-out returns after merge; gate not specially re-run |
| Nested fan-out / TUI multi-child chrome | Out of #72 |
| Classifier fan-out `DispatchAction` | Missing |

Consequence: Liberado is **no longer protected only by accident** — coding can fan out on purpose. Remaining width gaps are **entry surface** (face/tools) and **within-turn** parallelism, not “zero concurrent writers.”

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

| # | Gap | Status | What needs to happen |
|---|---|---|---|
| G-W1 | Classifier / MCP parallel dispatch | Open | Only if life-OS multi-MCP fan-out needs it; **not** required for coding S6 |
| G-W2 | Coding child sessions | **Done (#72)** hub path | Face entry + polish remain |
| G-W3 | Async face delegate | Open | Non-blocking `delegate` for chat; optional `delegate` → coding |
| G-W4 | Tool concurrency | Open | JoinSet for read-only tools only |
| G-W5 | Merge protocol | **Done (#72)** parent LLM merge | Parent gate / TUI review optional |
| G-W6 | Teardown | Partial | Fan-out removes worktrees; keep prune hygiene |
| G-W7 | Fast worktrees | Optional | CoW/pool only if scale demands |

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
| Mid-build coding resume | **Y** (#73) | Shadow-git per-attempt checkpoints + durable worktree park/resume; keep re-dogfood |
| `/rewind` file restore | **Y** (#73) | Shadow-git restore over workspace; keep recovery tests |
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
6. **~~Can_resume false after build starts~~** — **fixed on #73**; shadow-git checkpoints + durable worktree mean long coding jobs are now restart-safe. Keep re-dogfood as regression bar.
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
| Parallel coding fan-out + merge | Behind FOSS *product UX*; **mechanism landed #72** | **Partial** (was major gap) |
| Within-turn tool parallel | Behind OpenCode/Kimi | **Gap** (unchanged) |
| Face delegate → coding | Behind all three | **Gap** |
| Plan / explore **modes** | Presets on develop; behind FOSS approval UX | **Partial** |
| Interactive permission UX | Behind all three | **Gap** |
| Coding tool depth (LSP/graph/web) | Behind Grok/OpenCode | **Gap** |
| Checkpoints/rewind | On par after #73 (shadow-git + rewind); behind Grok/OpenCode polish | **Parity-capable** |
| Multi-surface coding apps | Behind OpenCode/Kimi | Accept or W1 later |
| Skills/plugins marketplace | Behind all three | **Medium** |
| Capability/trust topology | **Ahead** | Preserve |
| Project-root auth | Competitive (fail-closed allowlist) | **Parity** on develop |
| Daemon / multi-domain / Telegram | **Ahead** | Preserve |

---

## 11. Architect roadmap implications (ordered) — post-#76

Priority is for **coding harness performance + parity**, assuming life-OS P1 continues in parallel.

### Band A — Goal product completion

| # | Item | Status |
|---|---|---|
| 1 | Project authorization (S3) | **Done** (#66) |
| 2 | Plan / explore presets | **Done** (#67 / #68) |
| 3 | Self-host reliability | **Done** (#71) |
| 4 | Gate dogfood (S7) | **Open** — top remaining A-band quality lever |
| 5 | Goal surface polish | Partial (sidebar + tool stream; panes/diff UX open) |
| 6 | Plan approval UX + plan→build handoff | **Open** |
| 7 | Mid-build resume | **Done** (#73): shadow-git checkpoints + durable worktree park/resume |

### Band B — Width safely

| # | Item | Status |
|---|---|---|
| 8 | Merge-back protocol | **Done** (#72 parent LLM merge) |
| 9 | Coding hub children + worktrees | **Done** (#72, max concurrent **3**) |
| 10 | Live multi-subtask dogfood | **Open** — prove #72 under a real model |
| 11 | Face `delegate` → coding / async delegate | **Open** |
| 12 | Parallel **read** tools in executor | **Open** |
| 13 | Classifier MCP `dispatch_parallel` | Optional / lower priority for coding |

### Band C — Series loops

**Largest remaining design-ready, code-zero gap.**

14. Implement [`loops-plan.md`](../loops-plan.md) P1→P2→P3→P4.  
15. Ship `/loop` slash; dogfood a series.

### Band D — Continuity & chrome

16. ~~Checkpoints / `/rewind` (S4)~~ — **Done (#73).**  
17. Headless coding CLI packaging (`goal -p` + exit codes) — distinct from proven daemon API headless.  
18. Repo orientation beyond `list_symbols` + hashline edit (#76).  
19. AGENTS.md / skills discovery.  
20. Coding permission modes + diff review UX.  
21. TUI multi-child fan-out chrome.

### Band E — Economics & correctness

22. TE1 catalog narrowing.  
23. Attach `liberado-cost --json` to dogfood write-ups.  
24. Keep self-host + fan-out re-dogfood as regression bars.

### Explicit non-goals (for this parity track)

- Plugin marketplace clone  
- Image/video tools  
- Matching OpenCode desktop/enterprise console  
- Replacing life-OS daemon with single-process `cd && agent` only  

---

## 12. Suggested PR sequencing (for future implementers)

| Seq | Slice | Status | FOSS pressure |
|---|---|---|---|
| 1 | Project auth | **Done** | — |
| 2 | Plan + explore presets | **Done** | — |
| 3 | Dogfood reliability (#71) | **Done on develop** | — |
| 4 | Coding fan-out + merge (#72) | **Done on develop** | Grok/OpenCode width |
| 5 | Checkpoint + mid-build resume (#73) | **Done on develop** | Grok/OpenCode |
| 6 | Preflight gate (#74) | **Done on develop** | Self-PR quality |
| 7 | Coverage gap analysis + mutants (#75) | **Done on develop** | — |
| 8 | Hashline edit mode (#76) | **Done on develop** | Grok |
| 9 | Live dogfood of `subtasks` fan-out | **Next** | Honesty |
| 10 | Gate default-on + measure | Open | Grok goal verify |
| 11 | Plan approval UX | Open | All three |
| 12 | Face coding entry / async delegate | Open | All three |
| 13 | Loops P1–P2 | Open | Grok/Kimi |
| 14 | TUI diff + permission modes + headless CLI packaging | Open | Grok/OpenCode |
| 15 | Loops P3–P4 + series dogfood | Open | Grok/Kimi |

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
| Coding fan-out + merge | `crates/coder-agent/src/fanout.rs`, `crates/coder-sandbox/src/merge.rs` |
| Tools | `crates/coder-tools/src/lib.rs` |
| Attempt loop / critic / gate adapter | `crates/coder-agent/` |
| MCP parallel API (not coding S6) | `crates/orchestrator/` (`dispatch_parallel`) |
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

## 14. Bottom line (updated 2026-08-06 evening — post-#76)

### Where we are

On **`develop` `4fb57e1`**, Liberado closed four clusters the original analysis treated as open or half-open:

1. **Point at a real repo and ship a PR** — project auth, plan/explore presets, worktree isolation, git tools, self-host reliability (#71), proven re-dogfood (#70).
2. **Safe parallel width for coding** — hub-spawned worktree children + parent LLM merge-back (#72), concurrency default **3**. Architecture questions (where workers work / how merge / who resolves conflict) are **answered in code**, not only in the plan.
3. **Durable checkpoints + mid-build resume** — shadow-git snapshots per attempt + write-flush, durable worktree park/resume, rewind (#73). Long coding jobs are now restart-safe.
4. **Ship preflight gate** — generic `PreflightRunner` + CI-equivalent full matrix (#74); blocks terminal success until green. Hashline edit anchoring landed (#76).

Dogfood still taught the meta-lesson: **architecture ≠ performance** until a live run hits the bugs. Fan-out now needs the same honesty: **live multi-subtask dogfood** is the next proof, not more design.

### What still separates Liberado from FOSS day-to-day

| Rank | Gap | Why it still matters |
|---|---|---|
| **1** | **`/loop` series memory** | Design ready, **zero code** — largest remaining design-ready hole |
| **2** | **Gate default-on + bail taxonomy** | Maker≠checker still optional in practice |
| **3** | **Product chrome** (plan approval, diffs, permission modes, multi-child TUI) | Presets + fan-out exist; FOSS cadence does not |
| **4** | **Face entry + serial tools** | Fan-out is pack payload; chat still can't casually `task` a coding swarm; tools still serial |
| **5** | **Repo intelligence + CLI packaging** | Orientation efficiency + `goal -p` exit codes — not blockers for single PRs |

### Preserve

- Domain-agnostic goal kernel, deterministic verifiers, capability topology, multi-surface life-OS, daemon durability, isolation-before-fan-out discipline.

### Immediate next moves

1. **Live dogfood** of `payload.subtasks` fan-out under a real model (unit tests already green).  
2. **Gate measure / optional default-on** for coding profiles (S7).  
3. **Plan-approval chrome** or **face coding entry** (UX / width).  
4. **`/loop` P1** when recurring improvement is the product bottleneck.  
5. Keep **self-host + fan-out + checkpoint** re-dogfood as regression bars.  
6. Land develop stack onto **main** when ready.

**Baseline truth:** `develop` tip, dogfood write-up, PRs **#70–#76** — not "still open on main."