# vs-grok-build — Liberado vs Grok Build gap analysis (TUI for agentic coding)

**Status**: research note, 2026-07-21 (updated same day: `/goal` harness + `/loop`/cron).  
**Scope**: highest-leverage gaps if Liberado’s TUI is to be a *daily-driver agentic coding surface*, not a product-market comparison of “who wins coding agents.” Also covers **goal-achievement harness robustness** (Grok `/goal`) and **recurring work** (Grok `/loop` vs Liberado cron + series-loops plan).  
**Sources**: [xai-org/grok-build](https://github.com/xai-org/grok-build), [docs.x.ai/build](https://docs.x.ai/build/overview), [Background Tasks](https://docs.x.ai/build/features/background-tasks) (`/loop`), public writeups on goal mode (June 2026), Liberado architecture (`agentic-loops.md`, `session-surface-contract.md`, `positioning.md`, `current.md`, `verifiers.md`, [`loops-plan.md`](../loops-plan.md)), coding pack crates (`coder-*`), `liberado-cron`, TUI client (`crates/tui`).  
**Related**: [`vs-hermes.md`](vs-hermes.md) (life-OS / skills / cron gaps), [`../architecture/positioning.md`](../../spec/architecture/positioning.md) (coding is P3: *good enough + integrated*, not best-in-class), [`loop_architecture_reference_article.md`](archive/loop_architecture_reference_article.md).

---

## 0. Frame — they are not the same product

| | **Grok Build** | **Liberado** |
|---|---|---|
| **Job** | Terminal coding agent: open a repo, plan/search/build, ship diffs | Life OS: daemon + vault + chat + sessions; coding is one domain pack |
| **Default UX** | `cd project && grok` — in-process agent, fullscreen TUI | `liberado serve` + thin TUI client over HTTP/SSE |
| **Loop owner** | Shell/runtime inside the binary | Kernel (`executor` / session hub) + packs; TUI never owns the loop |
| **Trust model** | Local tools + permission prompts / always-approve / sandbox profiles | Capability sets, zones, proposals, human taps (Telegram/TUI) |
| **Stated bar (Liberado)** | Best-in-class coding harness | “Good enough + integrated” — joinable sessions on the same daemon |

This note **does not** recommend turning Liberado into a Grok Build clone. It asks:

> If I want *my* TUI for agentic coding day-to-day, where does Liberado still feel thin next to a purpose-built coding TUI — and which gaps pay back the most for the least architecture damage?

Grok Build is a **reference harness** (same spirit as Claude Code / Codex / OpenCode in `agentic-loops.md`). Steal patterns; keep the kernel.

---

## 1. Executive summary — top leverage ordered for *TUI coding*

If the goal is “open TUI → drive a coding session like a serious coding agent,” these are the gaps that matter most, **ranked by leverage for that job** (not by how flashy they are vs Grok Build’s whole product):

| Rank | Gap | Why it hurts TUI coding | Liberado today | Grok Build today |
|---|---|---|---|---|
| **1** | **Workspace-first “open a repo and code” path** | Coding agents are cwd-scoped; Liberado is vault/daemon-scoped. Starting a coding session is `/spawn coding <goal>`, not “this directory is the world.” | Coding pack has workspace root + path policy; composition is heavy | `cd repo && grok` |
| **2** | **Plan-first + human-visible plan + gated writes** | Without plan mode, the agent either over-edits or under-explains; TUI has nowhere to *review* intent before mutation | Optional planner inside coder-agent (config); not a first-class TUI mode | `/plan`, plan file auto-approve, other writes still ask |
| **3** | **Diff / artifact UX in the TUI** | Session stream shows events/tokens; coding *is* the diff. Without inline/stat/patch review, you cannot supervise well | `git_diff` / critic on evidence; TUI is chat+joined session, not a diff client | Inline diffs, plan review, artifact-forward TUI |
| **4** | **Mid-build resume / workspace checkpoints** | Long coding jobs die on restart or re-run and redo FS work | Intake resume shipped; **build resume explicitly open** (roadmap E6-c(b)) | Sessions/checkpoints/workspace layer as product core |
| **5** | **Permission UX that matches coding cadence** | Life-OS permission model (proposal + Telegram buttons) is correct for unattended work; for interactive coding it is slow | Zone grants, proposals, Telegram scopes | Per-tool ask / always-approve / plan-mode split; Shift+Tab modes |
| **6** | **Parallel subagents + isolation (worktrees)** | Multi-file / multi-hypothesis coding needs parallel search without trampling the main tree | Architecture: subagents + capability ∩; **worktree isolation not done** (coder-agent ARCHITECTURE) | Up to N parallel subagents; isolation as product feature |
| **7** | **Skills / project instructions discovery** | Coding agents compound via `AGENTS.md` / skills / repo conventions | Topology + prompts + MCP; no Grok/Claude-style skill folders | Skills, plugins, marketplaces, CLAUDE.md + AGENTS.md compatibility |
| **8** | **Headless coding CLI + machine I/O** | Scripts/CI and “run this fix and exit” are half of coding agent value | PR factory / evals / headless-ish server; not `liberado -p "…"` in a repo | `grok -p`, streaming-json, ACP stdio |
| **9** | **Rich coding TUI chrome** | Mouse, modes, queue, tasks, context meter, rewind — polish | Solid session client (switcher, join, answer, fork); chat-first | Fullscreen coding-first pager |
| **10** | **ACP / editor embed** | Optional later; IDE is not the TUI job | Named as future surface | `grok agent stdio` |

**Do not chase first (low leverage for *your* TUI coding goal):** marketplaces, image/video generation, billing/usage UI, Claude import modals, multi-tenant enterprise packaging. **Do not abandon Liberado strengths** while closing gaps: capability narrowing, joinable sessions, AskHuman, proposal gates, vault provenance, multi-surface (TUI + Telegram + HTTP).

### Also read (harness depth)

| Section | Topic |
|---|---|
| [§8 Goal harness (`/goal`)](#8-goal-harness-goal--loop-robustness--comparable-performance) | Condition-driven outer loops, verifiers, checkpoints, checklist UX — how Liberado matches Grok `/goal` *performance* |
| [§9 Recurring work (`/loop` vs cron)](#9-recurring-work--loop-vs-liberado-cron--series-loops) | Interval prompts vs one-shot cron vs series loops with memory |

---

## 2. Side-by-side capability map

### 2.1 Product shape

| Capability | Liberado | Grok Build | Gap severity for TUI coding |
|---|---|---|---|
| Fullscreen coding TUI | Yes (ratatui client) | Yes (primary product) | Medium — Liberado TUI is real but chat/session oriented |
| Daemon + remote surfaces | Yes (HTTP/SSE, Telegram) | Local-first; dashboard/ACP | Liberado *advantage* for life-OS; coding feels remote |
| One binary `cd && run` | No — needs vault + daemon + config | Yes | **High** |
| Headless one-shot | Partial (server APIs, evals, PR dispatch) | First-class `-p` | High for automation |
| ACP (editor protocol) | Planned / not product | Shipped | Medium (after TUI is good) |
| Multi-domain (life + coding) | Core identity | Coding-only product | Liberado *advantage* |

### 2.2 Agent loop & coding pack

| Capability | Liberado | Grok Build | Notes |
|---|---|---|---|
| Bounded tool loop | `liberado-executor` | Shell agent runtime | Both mature as harnesses |
| Coding tools | `list_files`, `search_text`, `read_file`, `write_file`, `edit_file`, `apply_patch`, `git_status`, `git_diff`, `run_command`, `validate` | File edit, terminal, search, web, … (Codex/OpenCode-influenced ports) | Liberado set is **sufficient for core edits**; thinner on web/LSP/terminal UX |
| Sandbox | Host + Docker specs; path/command policy | Sandbox profiles CLI flag | Liberado foundation OK; dogfood/ops thinner |
| Planner | Optional structured plan in coder-agent | Plan **mode** as UX + permissions | **UX gap**, not just “has a planner” |
| Critic / verifiers | Critic on git diff; config gates; progress guards | Arena / eval layers in product marketing; hooks | Liberado **strong** on verifier-as-code philosophy |
| Intake / criteria freeze | Intake session pack | Clarifying questions in plan mode | Liberado strong on maker≠checker |
| Repair + failure signatures | Yes | Generic multi-attempt | Liberado strong |
| Subagents | Kernel design + dispatch pack | Parallel subagents productized | Isolation (worktree) open on Liberado |
| Progress / doom-loop guards | Strong (`progress.rs`) | Harness-level | Liberado competitive |

### 2.3 TUI / surface

| Capability | Liberado TUI | Grok Build TUI | Gap |
|---|---|---|---|
| Chat + streaming | Yes | Yes | — |
| Session switcher (chat + goals) | Yes (`GET /api/sessions`) | Sessions/resume/fork | Liberado strong on unified model |
| Join live goal + answer AskHuman | Yes | Interactive ask | Liberado strong |
| `/spawn <domain> <goal>` | Yes | Domain model N/A (always coding) | Liberado multi-domain |
| Plan pane / plan mode | No | Yes | High |
| Inline diffs / review | No dedicated | Yes | **High** |
| Permission modes (ask / always / plan) | Capability + proposals, not coding-mode UX | Shift+Tab modes | High for interactive coding |
| Context usage meter | Status/model info | `/context` | Medium |
| Compaction / rewind | Fork from turn | `/compact`, `/rewind` | Medium |
| Task/subagent dashboard | Switcher + goals list | `/tasks`, dashboard | Medium |
| Skills as slash commands | Shared slash catalog | Skills → commands | Medium |
| Extensions modal (MCP/hooks/plugins) | Config/TOML + catalog API | First-class TUI modal | Lower for coding quality; higher for ops UX |
| Mouse-rich chrome | Some mouse handlers | Product emphasis | Polish |

### 2.4 Extensibility

| Capability | Liberado | Grok Build |
|---|---|---|
| MCP | First-class (topology, provenance) | First-class |
| Skills / markdown agent packs | Prompts + topology; no skill FS layout | `.grok/skills`, plugins, marketplaces |
| Project rules | Config + vault guidance | `AGENTS.md`, `CLAUDE.md`, `.claude/rules` |
| Hooks | Webhooks + EventSource model | Lifecycle hooks on tools/sessions |
| Claude Code compatibility | None | Explicit zero-config read of Claude assets |

---

## 3. Gap deep-dives (highest leverage first)

### Gap 1 — Workspace-first entry (cwd is the product)

**Grok Build:** The mental model is the repo. Launch is local; tools see that tree; sessions bind to directory.

**Liberado:** Mental model is the **daemon + vault + session hub**. Coding pack *does* take a workspace root and path policy, but the daily path is still “boot the life OS, then spawn a coding domain goal.” That is correct for integration; it is wrong for *feeling* like a coding agent.

**Why high leverage:** Every other coding UX (plan, diffs, permissions, resume) is easier when “current coding workspace” is a first-class, sticky context on the TUI — not only a field inside a goal payload.

**Concrete Liberado moves (in order):**

1. **TUI “coding workspace” context** — sticky cwd (or recent repos), shown in status bar; `/spawn coding` defaults to it.
2. **`liberado code [path]` or `liberado tui --workspace`** — composition root that starts/attaches daemon with coding pack pointed at that tree (still one kernel; less ceremony).
3. **Project discovery** — walk for `AGENTS.md` / `Cargo.toml` / `.git` to set root and load instructions (see Gap 7).

**Avoid:** forking a second agent runtime that bypasses session hub and capabilities.

---

### Gap 2 — Plan mode as a *permission + UX* mode, not only an optional planner call

**Grok Build:** Plan mode auto-approves plan-file edits; other writes still require approval. Plan stays visible. Clarifying questions before edits.

**Liberado:** Optional planner role in `coder-agent` can emit a structured plan into context. That is **model workflow**, not **operator mode**. The TUI does not expose “we are planning” vs “we are mutating,” and the permission system is not plan-aware.

**Why high leverage:** Plan mode is the cheapest way to make a weaker/mid coding agent *trustworthy* for interactive use. It also reuses Liberado’s strengths (AskHuman, proposals) with a coding-specific policy:

| Mode | Mutating tools | Plan artifact | Fits Liberado |
|---|---|---|---|
| Plan | Deny or proposal-only except plan file | Writable sticky plan note/session artifact | Capability narrow + pack flag |
| Act (default interactive) | Ask / grant once-session | Plan read-only | Existing grants |
| Always-approve | Auto | Optional | Session profile / flag |

**Concrete moves:**

1. Session attribute or profile: `coding_mode = plan | act | yolo`.
2. Coding pack enforces: plan mode → only allow writes under a designated plan path (or `append_note` plan section); other mutations → AskHuman or hard deny.
3. TUI: `/plan`, status chip, plan panel (even markdown scrollback is enough v1).
4. Keep optional LLM planner for “draft the plan”; mode gates *whether tools may escape the plan file*.

---

### Gap 3 — Diff and artifact surface in the TUI

**Grok Build:** Coding TUI is artifact-forward (inline diffs, plan review). Supervising means reading the change, not only the prose.

**Liberado:** Architecture explicitly says surfaces **may** render diffs/artifacts and **must not** own execution (`agentic-loops.md`). The **contract is there**; the TUI implementation is still mostly transcript + tool event names.

Coding pack already produces evidence:

- `git_status` / `git_diff` tools  
- Critic consumes unified diff  
- Session events stream tool start/finish  

**Why high leverage:** Without diff UX, you will not trust long coding sessions in-TUI; you will alt-tab to an IDE or to Grok Build. That is the defection condition.

**Concrete moves (v1 → v2):**

1. **v1:** On `tool_finished` for `apply_patch` / `edit_file` / `write_file` / `git_diff`, render a collapsible patch/stat block in the joined view (server can attach a short unified-diff snippet in the event payload if missing).
2. **v1:** `/diff` slash on a joined coding session → `git_diff mode=stat|patch` via API helper.
3. **v2:** Side panel: file list from last `git status --porcelain`; open hunk viewer.
4. **Do not** reimplement git in the TUI — call the same tools/APIs the agent uses.

---

### Gap 4 — Mid-build resume and workspace checkpoints

**Liberado roadmap (E6-c(b)) already names this:** intake can resume; the build loop cannot, because re-running redoes filesystem work. Suggested design: git commit (or stash/worktree marker) as suspend point.

**Grok Build:** Workspace/checkpoints are part of the product stack (`xai-grok-workspace`).

**Why high leverage for agentic coding:** Multi-hour or multi-attempt coding *will* hit restarts (homelab, laptop sleep, daemon deploy). Without checkpoints, “use Liberado for coding” loses to anything that rehydrates.

**Concrete moves:**

1. Design pass (no code first): checkpoint = `git commit` on a `liberado/session/<id>` branch or annotated stash; store commit SHA on session record.
2. On resume: pack loads criteria + last verifier failure + “continue from commit,” not “replay all tool calls.”
3. TUI: show checkpoint age/SHA on joined coding sessions; `/checkpoint` manual snapshot.

This is **pack + session store** work; TUI is a thin display.

---

### Gap 5 — Permission UX for interactive coding cadence

**Liberado’s permission model is a feature for unattended life-ops** (zone Write → proposal → Telegram Deny/Once/Session/Everywhere). For interactive TUI coding, the same path feels like friction unless:

- the human is already watching the stream (suppress OOB ping — already true when stream open), and  
- approvals are **in-TUI one-key**, not phone round-trips.

**Grok Build:** Permission modes are first-class TUI state (ask vs always-approve vs plan).

**Concrete moves:**

1. When TUI holds the goal stream, permission requests should surface as **in-band AskHuman / action chips** (not only Telegram).
2. Session profile for interactive coding: broader Write grant under workspace path policy; still no ambient host-wide shell.
3. `/always-approve` for *this session only* (maps to session grant expansion with clear audit), not a global YOLO.
4. Keep Telegram path for background coding jobs (cron, PR factory) — dual-path is correct.

---

### Gap 6 — Parallel subagents and worktree isolation

**Grok Build:** Parallelism is a headline (multiple subagents; product packaging around it).

**Liberado:** Kernel already thinks in child goals, capability ∩, budgets. Coding pack ARCHITECTURE lists **subagent / worktree isolation as not done**. Without isolation, parallel workers fight over one tree.

**Why leverage is high for *hard* coding tasks, medium for daily small tasks:** Solo sequential coding with a good plan mode may be enough for 80% of personal use. Parallelism becomes leverage when exploring large codebases or competing approaches.

**Concrete moves:**

1. **Worktree isolation** for child coding goals: create git worktree, run pack there, merge/PR result.
2. TUI: list child sessions under parent; join any; show isolation label.
3. Only then raise concurrency knobs — isolation before fan-out.

---

### Gap 7 — Skills and project instruction discovery

**Grok Build:** Discovers skills (project + user + plugins), plugins, hooks, MCP; reads `AGENTS.md` and Claude Code files with zero config. Skills appear as slash commands.

**Liberado:** Strong on **MCP + topology + capability grants**; prompts live in config/pack paths. Missing the **lightweight, repo-local markdown skill** layer that coding agents live on.

**Why medium-high leverage:** Closing this is mostly discovery + prompt injection + slash registration — not a new kernel. It makes Liberado behave well in *other people’s* repos and in multi-repo personal work.

**Concrete moves:**

1. On coding session start, walk cwd→root for `AGENTS.md` / `AGENT.md` / `.liberado/instructions.md` (and optionally `CLAUDE.md` for compatibility).
2. Load `./.liberado/skills/*/SKILL.md` (or reuse `.grok/skills` / `.agents/skills` read-only for interop).
3. Expose user-invocable skills as slash commands in TUI (same pattern as Grok).
4. Do **not** need a marketplace for personal leverage.

---

### Gap 8 — Headless coding CLI

**Grok Build:** `grok -p "…" --output-format streaming-json` is the automation story.

**Liberado:** Has headless *pieces* (server APIs, evals, PR-dispatch MCP, coder-runner) but not one sharp command:

```text
liberado code -p "fix the flaky test" --workspace . --always-approve
```

**Why high for agentic coding ecosystem, medium for pure TUI:** If TUI is the only entry, scripting and CI never use the same pack. One CLI that hits the same session API keeps the pack honest.

**Concrete moves:**

1. Thin CLI over existing `POST /api/goals` + stream, with workspace/profile flags.
2. Streaming-json mirroring `SessionEvent` wire kinds (already converged).
3. Exit codes from terminal status (`Succeeded` / `Failed` / …).

---

### Gap 9 — Coding-first TUI chrome (polish)

Grok Build invests heavily in pager UX: modes, queue, tasks, themes, context meter, rewind, export, vim scrollback, etc.

Liberado TUI already has real depth for **sessions** (switcher, join, fork, spawn, model select, mouse handlers). The gap is **not** “no TUI”; it is “TUI optimised for life-OS chat + session supervision,” not for continuous coding.

**Leverage after 1–5:** Polish compounds only when plan/diff/workspace exist. Otherwise you gold-plate a chat client.

**Pick later:** context meter, `/compact`, prompt queue, denser tool-call rendering, transcript search.

---

### Gap 10 — ACP / IDE embed

Shipped in Grok Build; named as future Liberado surface. **Defer** until TUI coding path is dogfoodable. ACP should be another **client of the session API**, not a second agent.

---

## 4. What Liberado already has that Grok Build is not optimised for

Do not trade these away while closing coding gaps:

1. **Capability boundary that only narrows** — self-extension cannot silently widen authority.
2. **Unified Session model** — chat and coding goals in one list; fork; park; AskHuman with idle budgets.
3. **Unattended + attended paths** — cron, webhooks, Telegram, TUI on one hub.
4. **Verifier-as-code + critic + progress guards** — philosophy aligned with serious harness design.
5. **Proposal / human provenance** — writes are attributable; daemon loop-break works.
6. **Multi-surface** — phone (Telegram) + laptop (TUI) without a second agent product.
7. **Life context** — vault, memory, tasks, briefs can *feed* coding goals (integration premium).

Positioning already says coding is P3 and “not replacing Claude Code / Grok Build.” This report is compatible with that: close gaps that make **integrated** coding *usable in the TUI*, not parity with every Grok Build feature.

---

## 5. Recommended program of work (TUI agentic coding track)

Assume life-OS dogfood continues as P1. This track is **parallel-safe** only where it hardens shared substrate (session events, permissions, workspace context). Otherwise sequence it when P1 stops wincing.

### Phase A — “I can supervise coding in the TUI” (highest ROI)

| Item | Outcome |
|---|---|
| A1 Workspace context | TUI + spawn know the repo; status shows cwd |
| A2 Diff/stat rendering | Joined view shows patches for mutating tools + `/diff` |
| A3 In-TUI permissions | Coding permission prompts answerable without Telegram |
| A4 Plan mode v1 | Mode chip + write gating to plan artifact; `/plan` |

**Exit criterion:** You prefer Liberado TUI over alt-tab for a 30–60 min coding task on your own repo.

### Phase B — “Long jobs survive reality”

| Item | Outcome |
|---|---|
| B1 Build checkpoints | Git-based suspend/resume for mid-build sessions |
| B2 Headless `code -p` | Same pack, machine-readable stream, CI-usable |
| B3 Instruction discovery | `AGENTS.md` + local skills loaded into coding sessions |

**Exit criterion:** Overnight/homelab restart does not force redo; a script can run the same agent.

### Phase C — “Hard tasks and interop”

| Item | Outcome |
|---|---|
| C1 Worktree subagents | Parallel children without tree fights |
| C2 Skills slash UX | Discoverable skills as commands |
| C3 ACP client surface | Optional IDE embed over session API |
| C4 TUI polish | Context meter, compact, queue, denser tool UI |

**Exit criterion:** Multi-step refactors with parallel research feel native; optional IDE attach.

### Phase D — “`/goal` performance + recurring work” (harness; see §8–§9)

| Item | Outcome |
|---|---|
| D1 Condition-driven outer loop | Continue while required verifiers fail; budget is a *cap*, not the story |
| D2 Goal templates + `/goal` surface | `tests-green` / free-text intake → frozen `VerifierSpec`s; TUI/CLI entry |
| D3 Checklist + verdict events | Partial progress visible; every outer cycle shows red/green checks |
| D4 Build checkpoints | Git tip + contract + checklist cursor; mid-build resume |
| D5 Series loops (L1–L6) | Implement [`loops-plan.md`](../loops-plan.md) on top of shipped cron |
| D6 TUI/Telegram loop ops | List / pause / close series; pass = ordinary joinable goal session |

**Exit criterion:** A template `/goal` reaches green without babysitting; a multi-day vault-grooming **series loop** compounds state across firings (not amnesiac cron).

---

## 6. Explicit non-goals (for this comparison)

- Matching Grok Build marketplaces, billing, image/video tools, or enterprise packaging.
- Replacing the daemon model with an in-process-only agent (loses Liberado’s multi-surface core).
- Auto-merging meta-loop prompt changes (Decision 14 stays).
- Peer agent meshes (rejected; see `channels-and-interactivity.md`).
- Making the TUI own the agent loop (surfaces stay clients).

---

## 7. Mapping to existing Liberado docs / tickets

| This report | Existing home |
|---|---|
| Mid-build resume | `docs/roadmap/current.md` E6-c(b) |
| Coding pack “not done” (worktree, Docker smoke, streaming) | `crates/coder-agent/ARCHITECTURE.md` |
| Surfaces render diffs; don’t own loop | `docs/architecture/agentic-loops.md` Surfaces table |
| Session surface obligations | `docs/architecture/session-surface-contract.md` |
| Coding is P3, good-enough + integrated | `docs/architecture/positioning.md`, `docs/roadmap/current.md` Priority 3 |
| Skills / self-extension (life-OS angle) | `docs/ideas/vs-hermes.md` §1 — different mechanism (`ProposeMcp` vs markdown skills) |
| ACP | Named future surface in architecture overview |
| Goal / turn / loop vocabulary | `docs/architecture/agentic-loops.md` §Vocabulary |
| Verifiers + criteria intake | `docs/architecture/verifiers.md` |
| Series loops (not yet built) | `docs/roadmap/loops-plan.md` (L1–L6) |
| One-shot cron (shipped) | `liberado-cron`, topology `[[schedules]]`, Telegram delivery |
| Cron dogfood / AskHuman crons | `docs/roadmap/current.md` Priority 1 (C1) |

---

## 8. Goal harness (`/goal`) — loop robustness & comparable performance

### 8.1 What Grok Build’s `/goal` is

Public product behavior (June 2026 goal mode + CLI helpers):

```text
grok goal "all tests pass and lint is clean"
  → plan / decompose (checklist)
  → act (optionally with subagents)
  → verify against the stated condition
  → if not met: replan / repair / continue
  → until condition holds | pause | budget | human clear
```

Controls people expect: **status / pause / resume / clear**. Mental model: *you leave; it keeps cycling until the predicate is true.*

That matches the industry convention Liberado already named in `agentic-loops.md`:

> `/goal` = success-based · `/loop` = time-based

| Property that makes `/goal` *feel* robust | Meaning |
|---|---|
| **Condition-driven** | Success is “predicate is true,” not “I finished N attempts” |
| **Long-running autonomous** | Outer cycle continues without a human turn each round |
| **Operator controls** | status / pause / resume / clear |
| **Verification in the loop** | Tests/lint/review *restart work*, not a postscript |
| **Decomposition** | Checklist / subgoals → visible partial progress |
| **Specialized roles** | Plan vs implement vs verify (sometimes as subagents) |

### 8.2 What Liberado already has (closer than it feels)

| Piece | Liberado status |
|---|---|
| Turn loop (bounded ReAct) | Production — `liberado-executor` |
| Goal session + domain packs | Production — `liberado-session` hub + `CodingSessionPack` |
| Criteria **intake** → freeze contract | Shipped (`intake_session`, mock e2e) |
| Frozen **verifiers** (paths, content, command, git diff) | Shipped (`verify_pipeline`) |
| Worker + **repair** + failure signatures | Shipped |
| **Progress guards** (read-only stall, same-tool, validation churn) | Strong |
| Optional planner + critic (maker ≠ checker) | Shipped (config) |
| Named terminals | Kernel vocabulary |
| Join / answer / park / cancel in TUI | Session surface is real |
| Mid-build **resume** | **Open** (roadmap E6-c(b)) |
| Worktree isolation for subagents | **Open** |
| Continuous “until predicate holds” outer driver | **Under-productized** |

**Liberado is not missing “a goal session.”** It is missing **`/goal` as a durable, condition-driven product mode** that keeps running until the *predicate* is true, with the dogfood reliability people feel in Grok.

Hierarchy (already documented):

| Term | Shape | Terminates when | Liberado home |
|---|---|---|---|
| **Turn loop** | model → tool → observe | report / prose / budget | `liberado-executor` |
| **Goal** | act → verify → repair | verifiers pass or named fail | session hub + packs |
| **Loop** | schedule → one improvement pass → sleep | **never succeeds closed** | planned — `loops-plan.md` |
| **Meta-loop** | evidence → config/prompt propose | human dispose only | heuristics-tuner |

### 8.3 Where robustness actually diverges

#### A. Success model: attempts vs condition

| | Liberado today (typical coding path) | Grok `/goal` (felt behavior) |
|---|---|---|
| Outer driver | `max_attempts` of plan/act/verify/repair | Keep going while condition is false |
| Stop reason people notice | Budget / max attempts / no-diff | “Tests green” or “paused” |
| User mental model | “Spawn a coding session” | “Hold this outcome until true” |

Intake already freezes `success_criteria` + `VerifierSpec`s — that *is* a condition. What is thinner is the **outer control policy**: re-evaluate the full verifier set as the primary continue signal; replan when stuck; treat budget as a *safety cap*, not the main story.

**Target outer loop:**

```text
while !verifiers.all_pass() && !terminal_budget && !paused:
    maybe_replan()
    attempt = worker_or_repair(last_verdict)
    verdict = run_verifiers()
    record_checkpoint()
```

#### B. Verifier quality and “live” predicates

Goals like `"all tests pass and lint is clean"` only work if conditions are **executable**, **re-run every outer cycle**, and failures become **structured repair input** (failure signatures — already present).

Gaps that hurt *performance*:

| Gap | Effect |
|---|---|
| Vague intake → weak verifiers | Soft “success” |
| Validation only at end of attempt | Wasted turn budget |
| No default “project green” profile | User invents specs every time |
| Critic overused as success signal | Feels flaky vs hard `cargo test` |

**Fixes:** goal templates (`/goal tests-green`); cheap mid-attempt smoke optional; hard gate order structural → process commands → critic last (critic never overrides hard fail).

#### C. Decomposition and partial progress

Grok “builds a checklist.” Liberado has planner + tools, but subgoals are not first-class with item-level verify, and progress is mostly mutation/validation churn—not “3/7 checklist done.”

**Fixes:** planner emits `GoalChecklist` artifact; outer loop works open items then re-runs **global** verifiers; TUI renders checklist; later map items → child sessions + worktrees.

#### D. Survival: pause / resume / checkpoint

Grok’s status/pause/resume/clear is ops *and* robustness. Liberado has park/AskHuman and intake resume; **mid-build resume is the known hole.** A long goal that cannot survive restart is not robust.

**Fixes:** checkpoint = git commit/tip + frozen contract + last verdict + checklist cursor; resume continues outer `while !pass`, does **not** replay all tools; expose `/goal pause|resume|status` (or map onto session park + “resume build”).

#### E. Turn-loop robustness (competitive)

Doom-loop guards, tool budgets, capability narrowing, policy denial — Liberado is not behind on design. Grok’s edge is less “smarter ReAct” and more **outer goal controller + verification culture + long-run ops**. Do not over-invest in another inner-loop rewrite.

#### F. Parallelism

Parallel subagents help search/fan-out; they do not replace a tight verify loop. Add worktree isolation **after** condition-driven outer loop + checkpoints.

### 8.4 Program to match `/goal` performance

**Layer A — Productize Goal mode (mostly policy + UX)**

1. TUI/CLI: `/goal <predicate-or-description>` → `POST /api/goals` with coding domain, autonomous profile, template id or free text.  
2. Always run intake (or expand template) → freeze verifiers.  
3. Outer driver in coding pack: continue while required verifiers fail and budget remains; full pass → `Succeeded`; repeated signatures → replan once then AskHuman / `NoProgress`.  
4. Session controls: status (checklist + last verdict), pause, resume, cancel.  
5. Default profiles: `goal-tests` (`cargo test` ± clippy), `goal-feature` (tests + nonempty diff + intake paths).

**Layer B — Verification as the performance engine**

1. Richer command verifiers (timeout, cwd, fail-snippet → repair).  
2. Verdict events on the wire every outer cycle (TUI red/green).  
3. Adaptive repair: same `FAILURE_SIGNATURE` thrash → escalate.  
4. Eval curriculum metrics: false success, time-to-green, interventions per goal.

**Layer C — Long horizon**

1. Checkpoints (E6-c(b)).  
2. Checklist state machine.  
3. Worktree subagents behind global verifiers.  
4. Time + token budgets separate from attempt count.

### 8.5 Mapping sketch

```text
Grok:  /goal "all tests pass and lint is clean"
Liberado:

  1. Template/intake freezes:
       VerifierSpec::Command { cargo test ... }
       VerifierSpec::Command { cargo clippy ... }

  2. Outer loop:
       attempt_i = worker/repair(prior_verdict)
       verdict = VerifierPipeline.run(all)
       if pass → Succeeded else continue / budget terminal

  3. Checkpoint after each attempt (git tip + verdict)

  4. TUI: checklist + last command outputs + status controls
```

### 8.6 Metrics for “comparable performance”

| Metric | Target |
|---|---|
| False success rate | Near zero when command verifiers required |
| False stall rate | Progress guards fire before burning max turns |
| Time-to-green (curriculum) | Competitive on *your* repos / models |
| Resume after restart | Mid-build continues without redo |
| Human interventions / goal | Low for templates; AskHuman only on ambiguity |
| Cost per success | Attempts drop when verify is early and signatures route repair |

Harness robustness is **these metrics**, not feature-checklist parity with Grok Build.

### 8.7 Bottom line on `/goal`

Liberado’s **loop design is already `/goal`-shaped**. The edge to close is the **productized outer controller**: condition-first, long-running, pause/resume, checklist, verify-until-true. Highest leverage: (1) condition-driven outer loop, (2) templates + frozen executable checks, (3) checkpoints + resume, (4) checklist + verdict UX, (5) only then parallel worktrees.

---

## 9. Recurring work — `/loop` vs Liberado cron & series loops

Grok’s **`/loop`** and Liberado’s **cron** are easy to confuse with each other and with **`/goal`**. They solve different problems. Liberado’s architecture already separates them; the gap is **product completeness of series memory**, not missing a timer.

### 9.1 Vocabulary (keep this table in your head)

| Kind | Question it answers | Stops when | Grok Build | Liberado today |
|---|---|---|---|---|
| **Goal** (`/goal`) | “Make this *true*” | Predicate holds (or hard fail/budget) | `/goal`, long autonomous run | Goal sessions + coding pack (under-productized as `/goal`) |
| **One-shot scheduled goal** | “At 7am, do *this once*” | That firing’s goal terminals | Partial (less life-OS oriented) | **Shipped:** `liberado-cron` + `[[schedules]]` → event → goal session; Telegram brief delivery |
| **Series loop** (`/loop`-class) | “Every interval, improve *this* a bit; remember last time” | Cap, green streak, or human close — **never “succeeded forever”** | `/loop 5m <prompt>` | **Planned:** [`loops-plan.md`](../loops-plan.md); **not built** — cron firings are amnesiac |
| **Background task / monitor** | “Run this process / watch this stream” | Kill / expire | `/tasks`, monitors, bg commands | Not a first-class TUI tasks pane; daemon sources instead |

From Grok docs ([Background Tasks](https://docs.x.ai/build/features/background-tasks)):

```text
/loop 5m Check if the test suite passes and report any failures
```

- Interval: `Ns` (min 60), `Nm`, `Nh`, `Nd`  
- **Fires immediately, then repeats**; each firing is a **new agent turn**  
- Expires after **7 days**; max **~50** concurrent scheduled tasks  
- Managed from **tasks pane** (`Ctrl+B` / `/tasks`); cancel there or via agent  
- Separate from: background shell commands, log **monitors** (line → notification), prompt **queue**

That is a **lightweight, chat-local, interval prompt scheduler** — excellent for “nudge me if tests break while I work,” not a full life-OS automation plane.

### 9.2 Liberado one-shot cron (already stronger for life-OS)

Shipped substrate:

| Piece | Role |
|---|---|
| `liberado-cron` | `EventSource`: cron expr → `Event` with goal text + pool |
| Topology `[[schedules]]` | Human-owned schedules; pool = authority |
| Daemon reaction | Firing → ordinary goal session on hub (joinable, cancellable) |
| Delivery | `Notifier` / `deliver_cron` → Telegram + sticky session fold-in |
| Capability | Schedule’s pool cannot exceed its grants |

**Strengths vs Grok `/loop`:** multi-surface delivery, durable daemon (survives TUI quit), capability pools, vault/MCP life tools, open-ended schedules (not 7-day chat session toys), same session model as interactive work.

**Weaknesses vs Grok `/loop`:**

| Gap | Effect |
|---|---|
| **No series memory** | Each firing is a stranger; cannot “continue grooming” from last pass changelog |
| **Config-authored, not slash-created** | No `/loop 5m …` in TUI; edit topology (or future `ProposeLoop`) |
| **No first-class tasks pane** | Operator uses logs/Telegram/session list, not `/tasks` |
| **No fire-immediately chat loops** | Cron is wall-clock; not “every 5m while I’m in this coding session” |
| **Overlap policy** | Cron has no series skip/queue semantics yet (loops plan: skip if pass still running) |
| **Interactive cron (AskHuman)** | Roadmap C1 — “ask if unsure” still thin for scheduled jobs |

Honest split:

- **Morning briefing / evening debrief** → Liberado **cron goal** (done / dogfooding).  
- **“Every 5 minutes check tests while I code”** → Grok **`/loop`** ergonomics; Liberado would need **session-scoped interval tasks** or a short-lived `[[loops]]` entry.  
- **“Keep this vault note tight over weeks”** → Liberado **series loop** (planned), not Grok’s 7-day prompt loop.

### 9.3 Liberado series loops (designed, not shipped)

From [`loops-plan.md`](../loops-plan.md) — architecture decision:

> **A loop is a scheduler for goals, not a fourth engine.**

```text
[[loops]]  schedule + goal template + checker + caps + stop_when
    ↓ cron fire
LoopSeries (durable)  pass_count, changelog, green_streak, status
    ↓
spawn ORDINARY goal session (template + artifact + changelog tail)
    ↓
verifiers = checker → append pass record → stop_when?
```

| Component | Maps to |
|---|---|
| Artifact | Vault note/doc (Decision-5 **loop-break**: loop’s own writes don’t re-trigger watch) |
| Checker | `VerifierSpec[]` per pass |
| Cap | max passes, per-pass budget, consecutive failures |
| stop_when | green_streak(N) \| cap \| human_close |
| Authority | schedule’s pool — no new capability story |

Settled: **skip if previous pass still running** (never unbounded queue); agent-created loops via **`ProposeLoop`**, not raw config writes (Decision 14).

**Implementation gaps L1–L6** (config, durable series, runner, context assembly, notify, surfaces) are listed in the plan — this report does not re-litigate them.

### 9.4 Side-by-side: Grok `/loop` vs Liberado cron vs Liberado series loop

| Dimension | Grok `/loop` | Liberado cron (shipped) | Liberado series loop (planned) |
|---|---|---|---|
| Trigger | Interval from slash command | Cron expression in topology | Cron + series state |
| Body | New agent **turn** (often same session context) | New **goal session** | New goal session **with changelog context** |
| Memory across firings | Weak / session-local | **None** (amnesiac) | **Changelog = series memory** |
| Lifetime | 7 days max | Until config removed | Active until stop_when / pause / close |
| Create UX | `/loop 5m …` in TUI | Edit `topology.toml` | Config v1; `ProposeLoop` later |
| Operator UX | Tasks pane | Session list + Telegram | `/api/loops*` + TUI list/changelog |
| Authority | Tool permissions / always-approve | Capability pool | Same as cron + per-pass budgets |
| Best for | While-you-work monitors, short recurrence | Life briefs, daily jobs, unattended | Multi-pass improvement of an artifact |
| Verify | Prompt-dependent | Pack verifiers if goal uses them | Checker **is** the pass verifiers |
| Delivery | In-conversation | Telegram + sticky chat + any Notifier | Notify on close/pause + pass sessions |

### 9.5 How Liberado builds comparable (and better) `/loop` performance

Do **not** clone Grok’s 7-day chat loop as the life-OS spine. Build three tiers:

#### Tier 1 — Keep winning at unattended cron (life-OS)

Already the P1 path. Raise robustness by:

1. **C1 AskHuman crons** — scheduled goals may pause for a real question; delivery already exists.  
2. **Richer schedule → verifier coupling** — briefing goals should fail closed on missing tools, not `PartiallySucceeded` silently (dogfood already taught this).  
3. **Schedule observability** — next fire times, last result, last session id in TUI/status API (Grok tasks pane energy without becoming chat-local).

#### Tier 2 — Ship series loops (the real `/loop` peer for Liberado)

Implement `loops-plan.md` P1→P4:

1. `[[loops]]` + durable `LoopSeries` + changelog under data dir.  
2. Runner on cron fire → spawn goal with changelog tail → update streaks → stop_when.  
3. Notify on close/pause (`max_consecutive_failures` → `Paused`).  
4. TUI: list loops, open last pass (ordinary join), read changelog.  
5. Dogfood: multi-day vault-note grooming loop (the plan’s acceptance test).

This is where Liberado can **beat** Grok `/loop`: durable series memory, capability pools, vault loop-break safety, multi-week life, Telegram when you’re away.

#### Tier 3 — Session-scoped interval tasks (optional coding ergonomics)

For Grok-like “every 5m check tests while I code”:

1. **Session-local scheduler** (or short-lived series loop bound to a coding workspace) created by slash `/loop 5m …` in TUI.  
2. Each tick spawns a **tiny goal** (or a restricted turn) with report-only tools; results append to the sticky coding chat or a side panel.  
3. Auto-expire (e.g. 24h or until session ends) — do not pretend this is life-OS automation.  
4. Cap concurrency; skip if previous tick still running (same policy as series loops).

This is polish for coding TUI dogfood, not a substitute for Tier 1–2.

### 9.6 Failure modes to design against

| Failure | Grok `/loop` risk | Liberado mitigation |
|---|---|---|
| Amnesiac redo/undo | Prompt-only recurrence | Series changelog (planned) |
| Feedback storm (edit → re-trigger) | N/A / tool-dependent | Decision-5 loop-break on vault writes |
| Queue stampede | Max 50 tasks; 7d expiry | Skip-if-running (loops plan) |
| Silent authority creep | YOLO / always-approve | Pool capabilities never widen mid-loop |
| Infinite spend | User cancels / expiry | Caps + stop_when + per-pass budgets |
| Human can’t see history | Scrollback / tasks pane | Pass = full goal session transcript + series changelog |

### 9.7 Bottom line on `/loop` / cron

| If you want… | Use / build |
|---|---|
| Daily life automation that pings the phone | Liberado **cron** (shipped) + delivery polish + C1 |
| Multi-pass improvement with memory | Liberado **series loops** (`loops-plan.md`) — highest leverage gap vs Grok for *life* |
| “Watch tests every 5m while coding” | Optional **session-scoped `/loop`** on TUI (Tier 3) — copy Grok UX lightly |
| Make a condition true once | **`/goal`** (§8), not `/loop` |

**Comparable performance for recurring work** is not “same slash command.” It is: **durable series state + one goal engine + capability-safe schedules + surfaces that can list/pause/join passes.** Liberado’s design already says that; shipping L1–L6 is the work. Grok’s `/loop` is a good *ergonomics* reference for Tier 3 and for a tasks pane, not the architecture Liberado should copy wholesale.

---

## 10. Bottom line

**Biggest real gaps for “use my TUI for agentic coding” are not “missing a coding loop.”** Liberado already has a serious coding pack (tools, sandbox hooks, planner/critic/repair, gates, progress control) and a serious session TUI (spawn, join, answer, fork).

The gaps are **productization of coding as an interactive craft**:

1. **Workspace-first entry**  
2. **Plan mode as permission UX**  
3. **Diff-first supervision in the TUI**  
4. **Checkpoints so builds resume**  
5. **In-band permissions at coding speed**  

And for **harness depth** (what makes Grok `/goal` and `/loop` feel robust):

6. **Condition-driven `/goal` outer controller** + templates + checklist/verdict UX (§8)  
7. **Series loops with changelog memory** on top of shipped cron — not amnesiac re-fires (§9)  
8. Optional **session-scoped interval tasks** for while-you-code monitors  

Everything else (parallel worktrees, skills interop, headless CLI, ACP, chrome) multiplies those. Closing Grok Build’s entire surface area is neither necessary nor aligned with Liberado’s positioning; closing these is what makes the existing kernel *feel* like a coding agent **and** a durable automation plane.

---

## 11. Appendix — tool surface snapshot (Liberado coding pack)

As of this writing, `CodingToolRuntime` exposes:

| Tool | Role |
|---|---|
| `list_files` | Inventory under policy |
| `search_text` | Exact text search |
| `read_file` | Read / line range |
| `write_file` | Full file write |
| `edit_file` | Single exact span replace |
| `apply_patch` | Multi-edit atomic apply |
| `git_status` | Porcelain status |
| `git_diff` | name_only / stat / patch |
| `run_command` | Policy-checked command |
| `validate` | Configured validation command |

This is enough for a strong sequential coding agent. It is **not** the bottleneck relative to plan/diff/resume UX. Add web search, LSP, or browser tools only when a real task fails for lack of them — not for parity optics.

---

## 12. Appendix — Grok Build reference layout (for pattern theft)

From the open repo / docs (names may drift with monorepo sync):

| Area | Grok Build |
|---|---|
| TUI | `xai-grok-pager` |
| Runtime | `xai-grok-shell` |
| Tools | `xai-grok-tools` |
| Workspace / VCS / checkpoints | `xai-grok-workspace` |
| Modes | Plan / always-approve / permission_mode config |
| Extensibility | Skills, plugins, hooks, MCP, marketplaces |
| Automation | Headless `-p`, streaming-json, ACP stdio |
| Interop | Claude Code + `AGENTS.md` discovery |

Steal: plan permission split, workspace binding, checkpoint ideas, skill discovery, headless event shape.  
Do not steal: product identity as “coding-only CLI” or marketplace gravity.
