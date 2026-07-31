# Agentic Coding TUI — Plan (goal-driven session surface + kernel completion gate)

**Status**: plan, 2026-07-24 — no code yet. Pulled forward from Priority 3 by owner decision
(2026-07-24): the daily-driver automation work continues in parallel; this is the chosen
frontend-adjacent track because the TUI already exists and the engine underneath it is done.
**Umbrella architecture**: [`agentic-loops.md`](../spec/architecture/agentic-loops.md) (kernel vs pack),
[`rust-native-agentic-coder-plan.md`](rust-native-agentic-coder-plan.md) (the coding pack roadmap —
this plan is its **surface + completion-gate chapter**, not a replacement),
[`loops-plan.md`](loops-plan.md) (`/loop` machinery), [`session-surface-contract.md`](../spec/architecture/session-surface-contract.md)
(what any session surface owes the user), [`verifiers.md`](../spec/architecture/verifiers.md) (intake +
frozen criteria), [`failure-modes.md`](../spec/architecture/failure-modes.md) (read before building).
**External references** (patterns, never code): Grok Build (`xai-org/grok-build`, Rust — the
completion-gate design), Claude Code (tool minimalism, plan mode, subagent isolation), OpenCode
(`sst/opencode` — TUI-as-client, per-mode tool permissions, snapshots), Kilo Code
(`Kilo-Org/kilocode` — shadow-git checkpoints, subtask lifecycle).

## The one-paragraph shape

A **goal-driven coding TUI** in pure Rust, on the framework we already own: the human types
`/goal <objective>` in the existing ratatui TUI; that starts an ordinary **goal session** on the
daemon (coding pack, authorized project root); the worker loop acts; and before the agent may
return to the human, a **completion gate** it does not control decides — deterministic verifiers
first, then a remembered **gatekeeper** critic and a quorum of **fresh cold reviewers** on
pack-assembled evidence (Grok Build's disputed-claim model, generalized). `/loop` schedules
recurring goals per `loops-plan.md`. Subagent delegation rides the existing hub. **Everything is
loosely coupled**: the gate, the loop scheduler, the event vocabulary, and the surface are
domain-agnostic kernel machinery — coding is the first pack to use them, not the only one that
ever can.

## Why now: the verified inventory (2026-07-24)

Verified against the code, not the docs' claims. The headline: **the engine is built.** What is
missing is the gate upgrade, the surface, and a handful of small kernel services (project-root
authorization, new wire events, the loop scheduler).

### Already landed (do not rebuild)

| Piece | Where | Notes |
|---|---|---|
| Turn loop (tools + budget + doom-loop guards) | `liberado-executor` | Production; `Budget`, doom-loop/cycle detection, escalation |
| Goal-session kernel (hub, packs, grants, terminals, resume) | `liberado-session` | `GoalSessionHub`, `SessionGrant` narrowing, `DomainPackRunner`; `Parked` exists today only as a restart artifact (first-class `hub.park()` is G2 work) |
| Coding pack: 10 discrete tools | `coder-tools` | `list_files, search_text, read_file, write_file, edit_file, apply_patch, git_status, git_diff, run_command, validate` — OpenCode-shaped, not vtcode-shaped |
| Coding pack: workspace/command isolation | `coder-sandbox` | `HostWorkspace` + `DockerWorkspace` scaffold; `PathPolicy` is workspace-relative already |
| Coding pack: the goal loop | `coder-agent` | intake → planner → worker → verifier pipeline → **critic** → repair; `max_attempts` + `prior_feedback`; progress guards |
| **A critic, already wired** | `coder-agent/src/critic.rs` | Reviews the **real git diff** (never the worker's narrative), JSON verdict `acceptable`/`needs_revision` + issues |
| Frozen acceptance criteria | `coder-core::{intake, verify}` + `GoalContract` | Grok's "acceptance contract" already exists as intake-frozen criteria + `VerifierSpec`s |
| One event vocabulary for chat + goals | `liberado-session` `SessionEventKind` + `chat-client-contract` wire mirror | `RoleStarted{role:"critic"}`, `ValidationFinished`, `LoopGuard`, `AwaitingInput`, `ToolStarted/Finished` — the wire already speaks roles and validation |
| Session streaming (catch-up + live) | `GET /api/goals/{id}/stream` | `subscribe()` returns history + broadcast receiver |
| The TUI itself | `crates/tui` (~7.2k lines) | Pure HTTP/SSE client; panes, slash palette, session switcher, model browser, themes, markdown |
| Shared slash-command crate | `liberado-commands` | `/spawn`, `/join`, `/fork`, `/session`… adding `/goal`/`/loop` is an enum + parser + handler |
| Chat context compaction | `liberado-main-agent` (CH3, 2026-07-23) | Persisted rolling-summary markers — the pattern goal sessions will reuse |
| Subagent delegation path | face `delegate` → hub → dispatch pack | Capability-narrowed child sessions, no `AskHuman` on delegated sessions (D-e) |
| Evals | `heuristics-tuner` `TUNER_LAYER=coder` + curriculum | Mock curriculum in CI; live tiers opt-in |
| **vtcode: already excised** | — | **Zero** workspace crates depend on vtcode. Remainder: one dead const (`VTCODE_BACKEND`, `coder-core/src/lib.rs:47`), one config comment, doc references, and the external `liberado-pr-dispatch-mcp` legacy backend (separate repo). Blast radius: delete a constant + comments |

### The gaps (what this plan builds)

| # | Gap | Kind |
|---|---|---|
| G1 | **Completion gate is single-reviewer and coding-internal.** One critic, one verdict, evidence hard-wired to `git diff`. No remembered gatekeeper, no quorum, no fail-closed verdict handling, no strategist on non-convergence. Not reusable by a non-coding pack. | kernel port + pack evidence |
| G2 | **No goal surface anywhere.** `/goal` doesn't exist as a command; the TUI can watch a goal session but can't start one against a project, can't show the role timeline as a first-class view, can't render diffs or verdicts. | surface |
| G3 | **No checkpoints/rewind.** Nothing snapshots the workspace per attempt; no restore. | pack service (coding) |
| G4 | **No project-root authorization.** `workspace_root` arrives as an opaque payload string; nothing proves the session may touch that directory. Today it defaults to a temp dir — safe but useless for real work. | kernel/policy |
| G5 | **Wire events missing for the gate + diffs.** No `critic_verdict`, `file_changed`, or `checkpoint` events (kernel enum + wire mirror + SSE map + TUI decoder — all four, per failure-modes §1). | kernel + contract |
| G6 | **`/loop` unimplemented.** Full design in `loops-plan.md` (L1–L6, slices P1–P5); zero code. | kernel + surface |
| G7 | **Coding subagent isolation.** `delegate` spawns dispatch-pack children; nothing spawns a *coding* child with its own worktree and merge-back. | pack |
| G8 | **Goal-session context compaction.** Chat compaction landed; executor turn loops inside goal sessions don't compact yet (follow-up already noted in `context-compaction-plan.md`). | kernel, later |

## What the references teach (and exactly where each pattern lands)

Patterns only. **No linking against Grok Build or VTCode crates; no JS/TS/Python anywhere in the
stack.** Grok Build is Apache-2.0 Rust — we read its architecture the way we read OpenCode's and
Kilo Code's; every line here is ours.

### Grok Build — the completion gate (the headline steal)

From the source teardowns (2026-07-16) and the goal-engineering reference:

- **`/goal` freezes observable acceptance criteria before implementation** — a contract of
  *observable outcomes*, a shared verification procedure, assumptions, risks, explicit non-goals —
  and deliberately does **not** freeze implementation architecture. → We already have this:
  intake-frozen `GoalContract` + `VerifierSpec`s. Keep it; document the non-goal section.
- **Completion is a disputed claim.** The implementer's final narrative *does not count as
  evidence*. Approval = a **remembered gatekeeper** (skeptic-0: resumes across attempts, catches
  "the same defect re-disguised") that may veto, **plus a strict majority of fresh cold
  reviewers** who must approve. → This is G1, generalized into the kernel (below).
- **Fail-closed verdicts.** Malformed, missing, or timed-out verdicts count as *refuting* votes —
  a sick reviewer can never lower the bar. → Adopt verbatim.
- **Strategist on non-convergence.** When repairs stop making progress, a separate role reads the
  plan + traces + deliverable and proposes **one structural change**; it cannot weaken the
  acceptance criteria, and its failure leaves completed work intact. → Adopt as the third gate
  role (config threshold, default after 3 rejected attempts).
- **`update_goal` tool + `/goal status|pause|resume|clear`.** → Our `submit_report` is the claim;
  the gate is the judgment. The slash surface lands in `liberado-commands` (G2).
- **The TUI is a client on a public wire** (ACP), with a leader process owning sessions. → We
  already have this shape (daemon + `chat-client-contract` + SSE). No ACP needed; our contract is
  the wire. Their **workspace crate** (host FS, VCS, execution, checkpoints) ≈ our `coder-sandbox`
  — the checkpoint home (G3).

### Claude Code — restraint and isolation

- **Minimal discrete tools** (Read/Write/Edit/Bash/Grep/Glob) — already our `coder-tools` shape.
- **Plan mode** = a read-only capability tier, not a different agent. → We already have the
  mechanism: capability narrowing per role/grant. A `plan` profile for the coding pack is config,
  not code (falls out of G4 + profiles).
- **Subagents get fresh context and filtered tools**; results return as one message. → Our hub
  child sessions + `SessionGrant` narrowing (G7).
- **`CLAUDE.md` project memory** → the vault is ours; project-root docs (`AGENTS.md`) are already
  injected by the PR factory's repo-context pattern — reuse for the coding pack's perceive step.

### OpenCode — client/server and per-mode permissions

- **TUI is a client of the server**, rendering a durable event stream (replay then live). →
  Already our exact shape (`subscribe()` catch-up + broadcast). The lesson to keep honoring:
  *rendering depends only on wire data* — no kernel types leak into the TUI (failure-modes §1).
- **Build vs Plan agents differ only in tool permissions** (`edit`/`bash`: allow vs ask). → Same
  point as Claude's plan mode: our capability boundary does this natively.
- **Declared subagents** (config files, per-agent tool filters) invoked via a Task tool. → Our
  `[[session_profiles]]` + policy grants are the declared-subagent registry; `delegate` is the
  Task tool (G7 wires the coding domain into it).
- **Snapshots + `/undo` `/redo`.** → G3's checkpoint service; `/rewind` in the TUI.

### Kilo Code — checkpoints and subtask lifecycle

- **Shadow-git checkpoints**: a side repo outside the project; `git write-tree` before and after
  each agent step; tree hashes stored with messages; restore = "files only" or "files + task";
  respects `.gitignore`. → G3 adopts this wholesale inside `coder-sandbox` (no project-history
  pollution, crash-safe, cheap).
- **`attempt_completion` tool**: the worker *signals* done; the harness then judges. → Already
  our `submit_report` → verifier pipeline → critic ordering. The gate (G1) hardens the judgment.
- **Subtasks can't spawn subtasks** (nested task tool disabled in child sessions). → Matches our
  delegated-session restrictions; keep one level deep in v1.

## The design

Loose coupling is the acceptance criterion of this plan, not a slogan. Rules (extending
`agentic-loops.md`'s dependency rules):

1. **The completion gate is a kernel port.** `liberado-session` owns *when* the gate runs and
   what its verdict means; the pack owns *evidence assembly* (coding: git diff + verifier
   results; a vault pack: artifact content + verifier results). A non-coding pack must be able to
   use the gate without importing anything git-shaped. That is the second-domain test for G1.
2. **The wire is the only surface contract.** The TUI renders `SessionEventKind` + JSON snapshot
   endpoints. New events land in **all four places** (kernel enum, `chat-client-contract` mirror,
   server SSE map, TUI decoder) — the 2026-07-11 convergence discipline; failure-modes §1.
3. **Config declares; agents never widen.** Project roots, loop definitions, quorum sizes, role
   models — config-owned. Agent-created loops ride `ProposeLoop` (settled in `loops-plan.md`);
   agent-created project roots don't exist.
4. **Packs depend on kernel; never on other packs.** Coding subagents are spawned *by the hub*,
   not by pack-to-pack calls.
5. **The TUI never owns the loop.** It starts, watches, answers, cancels, rewinds — via API.

### G1 — The completion gate (kernel port + coding evidence)

New module `liberado-session/src/completion_gate.rs` (kernel, domain-agnostic):

```rust
/// What a pack hands the gate. Coding: frozen contract + git diff + verifier verdicts.
/// A vault pack: frozen contract + artifact refs + verifier verdicts. No git types here.
pub struct GateEvidence {
    pub contract_summary: String,        // frozen acceptance criteria, rendered for a reviewer
    pub artifact_evidence: String,       // pack-assembled proof (diff / artifact content), capped
    pub verifier_verdicts: Vec<VerifierVerdict>, // deterministic results — already computed
    pub prior_refutations: Vec<String>,  // bounded history of past rejections (attempt memory)
    pub attempt: u32,
}

pub enum GateVerdict {
    Approved,
    Refuted { issues: Vec<String> },     // feeds prior_feedback on the next attempt
}

#[async_trait]
pub trait Reviewer: Send + Sync {
    /// One independent review. Err/timeout/parse-failure is handled by the GATE as a
    /// refuting vote (fail-closed) — reviewers never decide policy.
    async fn review(&self, evidence: &GateEvidence, fresh: bool) -> Result<ReviewVote, GateError>;
}

pub struct CompletionGate {
    pub fresh_reviewers: u8,        // default 2 — cold, stateless, see only GateEvidence
    pub quorum: Quorum,             // StrictMajorityOfFresh (default) — gatekeeper veto always on
    pub strategist_after: u32,      // default 3 consecutive refuted attempts
}
```

Flow per attempt (after the deterministic verifier pipeline — verifiers stay first, critic never
overrides a hard fail):

1. Pack assembles `GateEvidence` (coding: `git_diff_for_critic` moves here from `critic.rs`,
   plus the frozen contract render + verifier verdicts + bounded prior refutations).
2. **Gatekeeper** (remembered reviewer — its prior reviews ride `prior_refutations`) reviews.
   Refutation here is consequential: recorded, fed back, attempt ends.
3. **Fresh quorum**: N cold reviewers (fresh context, evidence only). Approval requires strict
   majority; ties/malformed/timeouts count as refutations.
4. `Approved` → the session may terminate `Succeeded` and return to the human. Anything else →
   `Refuted{issues}` → `prior_feedback` → next attempt (bounded by existing `max_attempts`).
5. After `strategist_after` consecutive refutations: **strategist** role (separate prompt/model;
   reads contract + trace + evidence; may not touch criteria) proposes one structural change,
   injected as the next attempt's directive.
6. Every vote emits a wire event (`critic_verdict { reviewer, kind: gatekeeper|fresh|strategist,
   verdict, issues }`) — the surface watches the gate work.

The existing `critic.rs` becomes the coding pack's reviewer adapter + evidence assembler; its
prompt/JSON-verdict parsing is reused for all three reviewer kinds. `CriticVerdict` stays the
pack-level vote type; `GateVerdict` is the kernel's.

**Why this is loosely coupled**: the gate sees `GateEvidence`, not a workspace. The second-domain
test: a "groom this vault note" goal assembles evidence as note content + `content_contains`
verifier verdicts — same gate, zero git.

### G2 — `/goal` and the goal surface (TUI)

`liberado-commands` additions (enum + parse + handlers, the crate's existing extension pattern):

| Command | Maps to |
|---|---|
| `/goal <text>` | `POST /api/goals` `{domain:"coding", description, payload:{workspace_root, ...}}` — project picked via `/goal in <project> <text>` or the TUI's current-project context (G4) |
| `/goal` (bare) | Open the goal view for the focused session |
| `/goal status` | `GET /api/goals/{id}` snapshot rendered inline |
| `/goal pause` / `resume` | `POST /api/goals/{id}/park` over a **new `hub.park()`** (graceful: finishes the in-flight turn, lands the session in `Parked` with its awaiting state intact — today `Parked` only exists as a restart artifact; making it a first-class transition is small but real hub work, not a free wrapper) / existing `resume` path |
| `/goal clear` | `POST /api/goals/{id}/cancel` (cooperative, existing) |
| `/rewind [n]` | `POST /api/goals/{id}/rewind` (S4 slice; restore checkpoint n, default last) |

TUI goal view (new focus mode, reusing the session-switcher + chat-render infrastructure):

- **Role timeline**: `role_started/finished` events render as planner → worker → critic segments
  with model names (the wire already carries them); tool chips as today.
- **Gate panel**: `critic_verdict` events render as reviewer ballots (gatekeeper / fresh-1 /
  fresh-2) with issues; the human watches the quorum vote.
- **Verifier panel**: `validation_finished` history with pass/fail.
- **Diff view**: `file_changed` events accumulate a changed-file list; Enter on a file fetches its
  diff (`GET /api/goals/{id}/diff?path=…` — thin endpoint over the workspace, capped) rendered
  with +/- theme colors.
- **Awaiting-input**: existing pattern — the input box goes hot for the prompt (intake questions,
  contract revision), per `session-surface-contract.md` (question + what it tried + what you said
  last, all visible without leaving the view).

### G3 — Checkpoints/rewind (coding pack, Kilo-style shadow-git)

In `coder-sandbox` (pack-owned; kernel sees only `checkpoint` events):

- `ShadowGit::init(workspace_root)` → side repo at `<LIBERADO_DATA_DIR>/checkpoints/<hash(root)>/`
  (bare; `git --git-dir=<side> --work-tree=<root>`).
- `snapshot(label)` at every attempt boundary (and before every `write_file`/`edit_file`/
  `apply_patch` flush — cheap `write-tree`, hashes kept in the session trace).
- `restore(tree_hash)` → checkout over the work-tree (files-only restore; conversation untouched —
  Kilo's "Restore Files Only" as the v1 semantic).
- Emits `checkpoint { id, label, tree_hash }` events; `/rewind` calls it through the API.

### G4 — Project-root authorization (policy, fail-closed)

`[[projects]]` in `topology.toml`: `{ name, root, write_class = "agent_writable" | "proposal_only" }`.
The coding pack's `workspace_root` payload must resolve inside a declared project root, or the
session fails fast (`PolicyDenied`). This is the zone model's fail-safe default applied to
arbitrary directories — an undeclared path is `proposal_only`, which for a coding session means
refused. The human typing `/goal in <project>` is the authorization *moment*; the config entry is
the authorization *fact*. `GET /api/projects` (thin) lets the TUI offer the picker. Subagent
children inherit the parent's project grant narrowed, never widened (Decision 4).

### G5 — Wire events (all four places, per addition)

| Event | Payload | Emitted by |
|---|---|---|
| `critic_verdict` | `{reviewer, kind, verdict, issues[]}` | the gate (G1) |
| `file_changed` | `{path, kind: create|modify|delete}` | coding tools on flush |
| `checkpoint` | `{id, label}` | G3 |
| `goal_progress` | `{message}` | worker's progress notes (Grok's `update_goal` telemetry; `Progress` exists — reuse it, no new variant) |

### G6 — `/loop` (per `loops-plan.md`, unchanged)

This plan does not redesign loops; it schedules them. Slices P1–P4 of `loops-plan.md` land as
written (config-authored `[[loops]]`, `LoopSeries` + changelog, cron-fired runner, `/api/loops*`).
The TUI adds `/loop` (list), `/loop <name> pause|resume|close`, and a series/changelog view —
a pass is an ordinary goal session, so the G2 surface renders it for free. Loop *definitions* stay
config-authored in v1 per `loops-plan.md`'s settled decision (`ProposeLoop` is the designed door
for agent-created loops); whether a human-typed `/loop new …` may also write one via a thin
`POST /api/loops` is settled at S5, defaulting to config-only.

### G7 — Coding subagents (hub-spawned, worktree-isolated)

> **Ordering constraint (audited 2026-07-24).** Isolation must land before *any* fan-out. Today
> nothing in production can dispatch two subagents at once — `dispatch_parallel` exists but is
> unreachable, `DispatchAction` cannot express fan-out, and the executor invokes tool calls
> serially — so the workspace race is prevented only by the *absence* of concurrency. Closing any
> one of those gaps without `WorktreeWorkspace` reproduces Bun's failure: agents sharing a git
> workspace overwrite each other, silently. Full audit and the fixed 3-step sequence:
> `docs/spec/architecture/agentic-loops.md` §Concurrency.

- `delegate` gains a coding route: when the face agent (or a coding worker, one level deep max)
  delegates with `domain:"coding"`, the hub starts a child coding session with a narrowed grant
  (project zone only, no `AskHuman`, no nested delegate).
- Isolation: `coder-sandbox` `WorktreeWorkspace` (git worktree per child under
  `<data>/worktrees/<session-id>/`). Merge-back: child produces a diff; the **parent's** next
  gate sees it as part of the parent's evidence (children never self-merge). Parallel children:
  v2; v1 is sequential with background visibility in the session list (the switcher already
  renders parent/child trees).

### G8 — Goal-session compaction (later, noted)

Chat compaction (CH3) proved the persisted-marker pattern. Executor turn loops inside long goal
sessions need a pack-level variant (tool-result pruning first, then marker compaction). Tracked
as a follow-up in `context-compaction-plan.md`; not a blocker for this plan's slices.

## Slices

Each slice: what lands, and the proof (per failure-modes doctrine — real objects, break-checks,
live runs where feasible).

| # | Slice | Proves it with |
|---|---|---|
| **S1** | **Completion gate kernel port** (`liberado-session::completion_gate`) + coding pack adoption (critic.rs → reviewer adapter; gatekeeper + 2-fresh quorum + fail-closed votes + `critic_verdict` events). Strategist included, config-gated. | Unit: quorum math (tie→refute, malformed→refute, gatekeeper veto). Integration over the real hub: mock reviewers — gate blocks `Succeeded` until quorum approves; refutation feeds `prior_feedback`. Break-check: silently approve-everything → test fails. |
| **S2** | **Wire events + TUI goal view v1**: `critic_verdict`, `file_changed` (kernel+mirror+SSE+decoder); `/goal <text>` against a temp-dir project; role timeline + gate panel + verifier panel; answer/cancel via existing endpoints; **`hub.park()`** + park/resume endpoints (first-class `Parked`). | Contract tests on the wire types; TUI reducer tests; park→resume integration test over the real hub; live dogfood: `/goal add a CLI arg parser to <scratch repo>` watched end-to-end in the TUI. |
| **S3** | **Project authorization**: `[[projects]]` config, fail-closed payload validation, `GET /api/projects`, TUI picker. | Config validation tests; API 403/PolicyDenied tests; live: undeclared dir refused by name. |
| **S4** | **Checkpoints**: shadow-git in `coder-sandbox`, snapshot per attempt + per write-flush, `checkpoint` events, `POST …/rewind`, TUI `/rewind`. | Workspace tests (snapshot→mutate→restore byte-identical, `.gitignore` respected); live: worker breaks a file, `/rewind`, file restored, session continues. |
| **S5** | **`/loop`**: `loops-plan.md` P1–P4 + `/api/loops*` + TUI loop list/series view. | The loops-plan's own acceptance: a 2-pass loop on the life pack closing on green streak; then a week-long vault-grooming dogfood. |
| **S6** | **Coding subagents**: delegate → coding domain, narrowed grant, `WorktreeWorkspace`, child diff into parent evidence. **`WorktreeWorkspace` is a hard prerequisite for any concurrency, not a component of it** — see `agentic-loops.md` §Concurrency, design rule 11. | Hub integration test (parent gate sees child diff); live: "split this feature into two subagent tasks" dogfood. |
| **S7** | **Strategist live + evals gate**: curriculum runs the gate (smoke tier in CI via mock reviewers; live tier opt-in); strategist fires on scripted non-convergence. | Curriculum accuracy stays 1.0 with the gate on; a scripted 3-refutation run shows the strategist directive in attempt 4's context. |

Ordering rationale: S1 first because the gate is the product claim ("the agent may not return
until an independent check agrees") and it is pure kernel+pack — no surface needed to prove it.
S2 makes it visible; S3 makes it safe against real repos; S4 makes it reversible; S5–S7 broaden.

## Non-goals (explicit)

- **No vtcode, no Grok Build crates, no JS/TS/Python.** Patterns are stolen; code is ours; the
  stack stays Rust end-to-end.
- **No new execution engine.** Turn loop (executor) ⊂ goal (hub+packs) ⊂ loop (scheduler) —
  unchanged. If a slice finds itself writing a fourth engine, it stops.
- **No ACP/leader retrofit.** Our daemon + `chat-client-contract` already is the public wire;
  Grok's lesson is to *keep* it that way, not to adopt their protocol.
- **No WebUI coding surface in this plan** (P2 chat surface stays the WebUI track; the contract
  events added here will make a future WebUI goal view mostly free).
- **No auto-merge, ever.** Gate approval returns work to the *human*; publication stays
  draft-PR/proposal-gated (Decision 14).
- **vtcode doc cleanup** rides along with S1's PR (delete the `VTCODE_BACKEND` const
  (`coder-core/src/lib.rs:47` + its mention in `coder-core/ARCHITECTURE.md:41`), the stale
  `config.example/tuning.toml` comment, and refresh the references in
  `rust-native-agentic-coder-plan.md` and `pr-dispatch-vtcode-no-write-finding.md` status), not a
  separate slice.

## Docs this plan updates when slices land

`agentic-loops.md` (gate row in the kernel table), `verifiers.md` (gate as the critic layer's
generalization), `session-surface-contract.md` (goal-view conformance rows), `overview.md` (crate
map: `completion_gate` module, checkpoint service), `reference/api.md` (`/api/projects`,
`/api/loops*`, park/resume/rewind/diff endpoints), `roadmap.md` (slice tracking).
