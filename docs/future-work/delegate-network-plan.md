---
kind: plan
status: draft
authority: advisory
domain: delegation
canonical_for: lan-delegation
open_items: true
---

# LAN delegation — a delegator agent driving a worker machine

**Status**: Plan, 2026-08-24. **D1 landed 2026-08-25** (`liberado-delegate-contract`,
`liberado-forge` with the Gitea impl, `liberado-worker` accept→worktree→run→push→PR,
`liberado delegate submit|status|cancel|health`; duplicate submit is a stored-record
no-op, restart rescan reports crashed runs honestly). D2+ not started. The rest of this
doc exists so the design survives until backlog capacity appears, and so it does not get
reinvented badly.

**Related**:

- [`ideas/a2a-protocol-idea.md`](ideas/a2a-protocol-idea.md) — orchestrator + narrowed-workers is
  the confirmed model; peer authority-splitting stays out.
- [`../spec/inbox-spec.md`](../spec/inbox-spec.md) — vault capture inbox. Different inbox, same
  patterns (idempotency, settle windows, terminal states).
- `crates/cli/src/shepherd_cmd.rs` — the ready-or-blocked PR loop this extends.
- `crates/coder-agent/src/fanout.rs` — local child runs; remote children are the same shape.
- [`paseo-liberado-integration-roadmap.md`](paseo-liberado-integration-roadmap.md) Phase 6 —
  Track B remote access. Delegation rides the same transport decisions but is not that feature.

---

## 1. Goal

One Liberado machine (the **delegator**) hands coding tasks to another Liberado worker on the
same network (the **worker**). The delegator defines the work and its acceptance gates. The
worker runs the coding pack in its own worktree, opens a pull request, and monitors it. If the
worker hits a blocker, it asks a question. Questions and finished PRs land in the delegator's
inbox. The delegator wakes when the inbox is non-empty, drains items in arrival order, reviews,
and either merges or kicks the work back.

The measure of success is boring: the delegator is woken rarely, each wake is small, and merged
work clears every gate the delegator set.

## 2. Non-goals

- **No peer-to-peer authority.** The worker never negotiates with the delegator or with another
  worker. The delegator holds all authority; the worker executes inside a narrowed grant
  (the A2A research verdict).
- **No new agent engine.** The worker runs the same coding pack, executor, gates, and traces as a
  local run. "Different runtime + different verifiers" is banned by the architecture contracts.
- **No inbound tasks.** Other systems cannot hand work *to* the delegator here. That is the
  external A2A bridge idea, kept separate.
- **No internet exposure.** LAN only, explicit endpoints in config, token auth. No discovery.

## 3. What already exists (reuse map)

| Need | Existing machinery | Where |
|---|---|---|
| Bounded agent loop for delegated work | `Executor::execute` ends with a typed `Report` (`SUBMIT_REPORT_TOOL`) | `crates/executor` |
| Child coding runs with grants | `run_coding_fanout`, `child_session_grant`, `CodingSubtask { label, description, success_criteria }` | `crates/coder-agent/src/fanout.rs` |
| One seam for "run a coding task" | `trait CoderBackend { async fn run(CoderRunRequest) -> CoderRunResult }` | `crates/coder-core/src/lib.rs:1314` |
| Per-run isolated worktree | `durable_session_workspace` → `coding-worktrees/<session_id>` | `crates/coder-sandbox` |
| Acceptance command gates | `PreflightSpec` / `PreflightStep`, differential against base | `crates/coder-sandbox/src/preflight.rs` |
| Ready-or-blocked PR state machine | shepherd labels `shepherd:ready` / `shepherd:blocked` / `shepherd:ci-rerun`, kickback caps, cold reviews | `crates/cli/src/shepherd_cmd.rs` |
| Reviewer judgment | `cold_review` module, `COLD_DIFF_REVIEWER_PROMPT`, bounded fix rounds | `crates/coder-agent` |
| Budgets | `Budget`, `TokenLimit`, `WallClockLimit` | `crates/executor` |
| Idempotent events | `correlation_id` convention (Decision 6) | vault watcher, hooks |
| Wake-on-event | `EventSource` narrow waist; everything fans into one daemon channel | `crates/daemon` |
| Wire DTO precedent | `chat-client-contract` — DTO-only client crate, no transport logic | `crates/chat-client-contract` |

The only genuinely new pieces are: a wire contract between the two machines, a worker daemon,
a remote `CoderBackend` implementation, an inbox spool, and forge operations behind one trait.

## 4. Topology

Two roles, asymmetric on purpose:

```
delegator machine                          worker machine ("bench-box")
┌─────────────────────────────┐            ┌──────────────────────────────┐
│ liberado daemon             │            │ liberado-worker (axum HTTP)  │
│  ├─ delegate supervisor     │  HTTP/SSE  │  ├─ task queue (durable)     │
│  ├─ inbox spool + adapter   │◄──────────►│  ├─ coder pack assembly      │
│  └─ (orchestrator dispatch) │            │  │   └─ executor loop        │
│                             │            │  ├─ worktrees per session    │
│        git push/pull, PR API│            │  └─ shadow-git checkpoints   │
│ ┌───────────────────────────┼──git/REST──┼──► forge (Gitea / GitHub)    │
```

- The **worker hosts the control plane**. The delegator is a pure HTTP/SSE client of it. This
  keeps "every surface a client of daemon APIs" true, adds no inbound server to the delegator
  box, and matches the house SSE style.
- The **forge holds the artifacts**: branches, commits, PRs, CI checks, review comments. Nothing
  large travels over the control plane. A PR URL is the deliverable.
- Both machines run full Liberado config stacks. The worker's own `policy.toml` and grants still
  apply — the delegator's grant can narrow further, never widen.

## 5. The contract

New crate `liberado-delegate-contract` (client role): DTOs and route constants only, no transport,
mirroring `chat-client-contract`. Serde types:

```rust
struct TaskSpec {
    id: TaskId,                 // ulid; idempotency key for submit
    project: String,            // names a [[projects]] entry on the worker
    repository: String,         // OWNER/REPO or Gitea path; clone URL resolved by worker config
    base_branch: String,
    goal: String,               // the full description of what needs doing
    success_criteria: Vec<String>,          // same shape as CodingSubtask.success_criteria
    acceptance: Acceptance,     // §6
    budget: TaskBudget,         // max_turns, wall_clock_secs, token_cap
    grant: TaskGrant,           // branch namespace, max_kickbacks, forbidden_paths
}

struct Question {                // worker -> delegator
    id: QuestionId, correlation_id: String,
    task_id: TaskId, session_id: String,
    body: String,               // what is blocking, what was tried
    options: Vec<QuestionOption>,   // each: label + consequence
    default_option: Option<String>, // what the worker does if the answer times out
}

struct Answer { question_id: QuestionId, chosen_option: Option<String>, body: String }

struct WorkerEvent { kind: EventKind /* Question | StatusChanged | PrReady | Blocked */,
                     task_id: TaskId, payload: serde_json::Value }
```

Routes (all under `/v1/delegate`, all auth-token protected):

| Route | Direction | Purpose |
|---|---|---|
| `GET /health` | delegator → worker | liveness + build fingerprint |
| `POST /tasks` | delegator → worker | submit; idempotent on `TaskSpec.id` |
| `GET /tasks/{id}` | delegator → worker | status poll (reconciliation path) |
| `GET /tasks/{id}/events` | delegator → worker | SSE stream of `WorkerEvent`s |
| `POST /tasks/{id}/answers` | delegator → worker | reply to a question, or a kickback instruction |
| `POST /tasks/{id}/cancel` | delegator → worker | cooperative stop at the next tool boundary |

Delivery semantics are at-least-once everywhere. Every message carries a `correlation_id`;
receivers deduplicate. This is the same discipline the vault inbox uses.

## 6. Acceptance gates travel in the task

The delegator states the bar up front; the worker cannot lower it.

```rust
struct Acceptance {
    preflight: Vec<PreflightStep>,   // run on the worker before the PR opens (ship bar)
    required_checks: Vec<String>,    // forge CI check names that must pass (shepherd check_names)
    forbidden_paths: Vec<String>,    // diff must not touch; also given to the reviewer prompt
}
```

Three independent layers:

1. **Local ship bar** — the worker maps `acceptance.preflight` onto `PreflightStep`s and runs the
   existing differential preflight against the base commit. A red gate means no PR.
2. **Forge CI** — `required_checks` are matched against check runs on the PR, exactly as the
   shepherd matches `check_names` today. Branch protection is optional; homelab Gitea may not
   have it, so the delegator verifies checks itself before merge rather than trusting the forge.
3. **Cold review** — when the PR goes green, the *delegator* runs the existing cold-review
   pipeline over the diff before the item becomes reviewable. The reviewer model sees
   `success_criteria` and `forbidden_paths`.

## 7. Worker lifecycle

1. **Accept** `TaskSpec` after checking: token valid, project known, disk above the floor,
   concurrency slot free, build fingerprint current. Otherwise reject with a reason — honestly,
   per the harness rule that infrastructure failure is reported, not improvised around.
2. **Clone/worktree** — the worker keeps one bare or normal clone per repository and creates
   `delegate-worktrees/<task_id>` from `base_branch`. Fresh clone recipe pins
   `core.autocrlf=false`; Windows line endings stay a known trap.
3. **Run** — assemble the coding pack exactly as a local fan-out child does:
   `child_session_grant` narrowed by `TaskGrant`, `Budget` from `TaskBudget`, executor in report
   mode, coder-traces written per turn on the worker.
4. **Submit** — push branch `delegate/<task-short>/<slug>`, open the PR with a body carrying
   `task_id`, `success_criteria` as checkboxes, and the preflight report summary.
5. **Monitor** — watch required checks and the PR. On green plus no unresolved threads, emit
   `PrReady`. On failing CI it owns, retry once via the shepherd rerun pattern, then emit
   `Blocked`.
6. **Finish** — on merge ack from the delegator, remove the worktree and archive the session.
   The branch tip lives on the forge; the worker holds nothing irreplaceable.

A `RemoteCoderBackend` on the delegator implements `CoderBackend` by speaking this protocol. That
makes a remote machine just another target for `run_coding_fanout` — local children and remote
children share one code path, and no kernel type changes.

## 8. Blockers: the question protocol

When the worker's run cannot proceed, it calls a `ask_delegator` tool (offered only when the task
carries a delegator endpoint). The tool takes structured options, not free text alone — the model
must propose answers, which makes the delegator's job a choice instead of a research task.

On ask:

1. The worker persists session state (message history JSONL, checkpoint ref, worktree path) and
   parks the loop. No busy-wait, no process held hostage: resume reloads messages into the
   provider history and continues the same executor conversation.
2. The question becomes a `WorkerEvent` on the SSE stream.
3. If no answer arrives within the tuning timeout, the worker takes `default_option` if present,
   else files `Blocked` and ends the attempt. The worktree and branch survive either way.

The delegator's reply travels as `Answer`; the worker injects it as the next user turn. Resume
correctness gets the same proof discipline as everything else here: break it, watch the test
fail, restore it.

v1 fallback if park-and-resume proves harder than expected: end the attempt with `Blocked`,
then start a fresh run seeded from the same branch plus the answer appended to the goal. Worse
context, same durability. Decide by measurement, not preference.

## 9. The delegator inbox

Physically: an append-only spool under the data dir — `delegate-inbox/items.jsonl` plus a small
index of pending sequence numbers. Not the vault; this inbox is machine-local and agent-addressed.

An adapter turns SSE events into spool items and emits into the daemon's event channel — the same
`EventSource` seam the vault watcher and hooks use, so waking needs no new mechanism:

- An event arrives → item appended (`{seq, kind, task_id, correlation_id, payload}`) → daemon
  notified → orchestrator dispatches "drain the delegate inbox" to the delegating agent.
- If the daemon was down, the next activation drains the backlog first. FIFO by `seq`; order
  within a task is strict, order across tasks is arrival order.
- Duplicate delivery is harmless: `correlation_id` dedupe on append.

Not bothering the delegator too often is a design requirement, not a hope:

- **Settle window** (default ~90 s): bursts of questions from parallel tasks group into one wake.
- **Standing policies** (later phase): a small policy file lets whole classes of questions be
  answered without a model call ("dependency minor bumps: yes"; "schema changes: no"). The worker
  sees these as pre-answered defaults, so many questions never get asked at all.
- **Caps**: `max_open_questions` per task; exceeding it converts further questions into `Blocked`
  with the questions recorded. The inbox reports one blocked task, not twenty.

Inbox item kinds: `Question`, `PrReady`, `Blocked`, `Note` (milestone pings, off by default).

## 10. Review, kickback, merge

`PrReady` wakes the delegator. It then:

1. Verifies locally what the forge claims: checks green, diff surface sane, no forbidden paths.
2. Runs the cold review over the diff (existing module), bounded rounds like the shepherd's
   `MAX_FIX_ROUNDS`.
3. Decides: **merge**, **kick back**, or **escalate** (to the human).

Kickback = one action, two records: a review comment on the PR (durable, human-visible audit) and
an `answers` call to the worker with `kind=instruction`, referencing the kickback round. The
worker resumes the same session on the same branch with the instruction. Rounds cap at
`grant.max_kickbacks`; past the cap the task lands as `Blocked` for the human. This mirrors the
shepherd kickback economics exactly, because it is the same situation.

Merge happens through the forge abstraction (§11) by the delegator only. After merge ack, the
worker cleans up. The task record closes with the PR URL as its artifact trail.

## 11. Forge abstraction

New crate `liberado-forge` (client role). One trait, two implementations:

```rust
#[async_trait]
pub trait ForgeClient: Send + Sync {
    async fn open_pr(&self, req: OpenPr) -> Result<PrRef, ForgeError>;
    async fn comment(&self, pr: &PrRef, body: &str) -> Result<(), ForgeError>;
    async fn checks(&self, pr: &PrRef, names: &[String]) -> Result<CheckStates, ForgeError>;
    async fn merge(&self, pr: &PrRef, method: MergeMethod) -> Result<MergeCommit, ForgeError>;
    // plus branch helpers used by setup
}
```

- **GitHub**: keep shelling out to `gh` first (current shepherd precedent), move to REST later.
- **Gitea**: plain REST over `api/v1` with a token. Gitea mirrors the needed GitHub surface
  closely enough for branches, PRs, comments, statuses, and merge.

The shepherd migrates onto this trait opportunistically; it is not blocked on delegation and
delegation does not wait for a full migration.

## 12. Config surface

`topology.toml` gains one section; nothing else moves:

```toml
[[delegates]]
name = "bench-box"
endpoint = "http://192.168.1.50:7780"
token_ref = "env:LIBERADO_DELEGATE_BENCH_TOKEN"  # secret_ref grammar, same as [[hooks]]
max_concurrent_tasks = 2                          # in flight from this delegator
```

Existing sections grow fields, following the established patterns:

- `[shepherd.projects]` entries gain `delegate = "<delegate-name>"` and `forge = "gitea"|"github"`.
- `tuning.toml` gains `[tuning.delegate]` as an opaque value parsed by a
  `DelegateTuning::from_value` — the exact pattern `[tuning.coder]` uses for `CoderTuning`.

Per AGENTS.md: any new `CoderTuning` field must reach every `CoderRunConfig { ... }` initializer,
and `config_literal_rules.rs` is the mechanical guard to extend, not care to rely on.

## 13. Crate placement (layer rules)

All new crates declare `[package.metadata.liberado] role` or `layer_rules.rs` fails the build.

| Crate | Role | Why |
|---|---|---|
| `liberado-delegate-contract` | client | DTOs only; depended on by both sides; no transport deps |
| `liberado-forge` | client | trait + gh/Gitea impls; usable by cli, worker, future surfaces |
| `liberado-worker` (library) | service | axum host wiring contract ↔ coder pack assembly |
| `liberado-worker` (binary) | root | composition root; concrete notifier/forge choices appear here |
| `RemoteCoderBackend` impl | pack | lives beside `fanout.rs` in `coder-agent`; implements a pack-layer trait |
| `liberado-inbox-spool` | store | append-only queue + index; depends down only; daemon adapts it into an `EventSource` |

Nothing in kernel/config/store learns about HTTP endpoints or forges. The dispatcher stays
untouched in the early phases; making delegation a dispatcher-visible capability (a narrowed
subagent that happens to be remote) comes later, gated by the capability-narrowing rules the A2A
note requires.

## 14. Failure modes, answered in advance

| Failure | Answer |
|---|---|
| Worker offline at submit | Task queues in the delegator spool; delivered on next health-ok. |
| Worker dies mid-run | Worktree + session JSONL persist on the worker; on restart it rescans open tasks in its namespace and resumes or reports `Blocked`. |
| Network partition during park | Question delivery retries with backoff; timeout default-option rule bounds the stall; reconciliation sweep catches missed events. |
| Duplicate submit / replayed event | Idempotency keys + `correlation_id`; second copy is a no-op. |
| Budget overrun | Executor `Budget` kills the loop; result files as honest failure, not partial success. |
| Disk exhaustion | Worker refuses new tasks under a free-space floor; a running task that hits ENOSPC fails as infrastructure, ending the run per PR #166. |
| Stale worker binary | `/health` returns a build fingerprint (crate version + git describe); supervisor logs mismatches loudly. Reinstall discipline stays manual, as documented. |
| Rogue poster on the LAN | Token per delegate pair, constant-time compare; unknown tokens rejected; worker accepts tasks only from configured delegators, fail-safe like unlisted zones. |
| Worker merges something bad | Workers cannot merge. Merge is delegator-only, after gates. Branch protection where available is belt; delegator-side verification is braces. |

## 15. Build order

PR-sized slices, each with mutation evidence for behavior claims, per the repo rules.

| Phase | Slice | Acceptance |
|---|---|---|
| **D1** | `liberado-delegate-contract` DTOs + `liberado-forge` with Gitea impl; worker skeleton: accept task → fresh worktree → run pack headlessly → push branch → open PR. Submit by CLI. | One real task completes end-to-end against homelab Gitea; traces exist on the worker; duplicate submit is a no-op. |
| **D2** | SSE events + delegator inbox spool + wake/drain; `ask_delegator` with park/resume. | A question round-trips while the delegator agent is idle; FIFO drain proven; resume test passes both ways (mutation applied and observed). |
| **D3** | Acceptance plumbing: `preflight` steps mapped, `required_checks` verified delegator-side, cold review before `PrReady` handling, kickback rounds with caps. | A deliberately failing gate produces a kickback, then a fix, then merge; cap overflow produces `Blocked`. |
| **D4** | Shepherd integration (`delegate` field), standing policies, digests/settle tuning, multi-worker pools, `RemoteCoderBackend` wired into fan-out for mixed local/remote children. | Two workers take interleaved tasks without cross-talk; a mixed fan-out lands one local + one remote branch. |

## 16. Measures

Traces stay on the worker; inbox items carry session ids and links, so every number below is
derivable without new instrumentation:

- questions per accepted task (lower is better; standing policies should bend this curve),
- kickbacks per task, time-to-ready p50/p95,
- delegator wakes per day and median items per wake (the "not bothered too often" metric),
- human interventions per week (target: rare escalations only),
- cost per merged task, using the existing cost journal shape.

## 17. Open questions

1. Should questions also mirror into PR comments for a single audit trail? Lean yes at D3;
   costs one forge call per question.
2. Does a remote child count toward dispatcher subagent economics in A1/A2 measurement? Decide
   before D4 so the counters agree.
3. mTLS versus bearer tokens long-term. Tokens are proportionate for a home LAN; revisit if any
   segment leaves it.
4. Interactive attach (ACP-style live view of a remote session) — natural extension of the same
   control plane, but not needed for the loop above. Keep out of scope until D4 proves demand.
5. Where the delegating agent's "drain inbox" goal lives: a scheduled catch-up sweep exists
   regardless; whether an always-on immediate-wake dispatch is worth its complexity is a D2
   measurement call.
