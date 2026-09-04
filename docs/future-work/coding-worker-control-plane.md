---
kind: plan
status: draft
authority: advisory
domain: coding-harness
canonical_for: coding-worker-control-plane
open_items: false
---

# Coding-worker control plane

**Status**: draft. Not scheduled. Not selectable from the backlog.
**Evidence gate**: the published C3 baseline in
[`cross-harness-baseline.md`](cross-harness-baseline.md).
**Source**: conversation captured in
[`research/agent-orchestration-idea-from-Sol.md`](research/agent-orchestration-idea-from-Sol.md).

## 1. One sentence

Liberado does not need to win the coding-loop contest. It needs to **operate coding agents**.

The split:

- Liberado owns scheduling, events, task state, routing, retry policy, and supervision.
- Codex / Claude Code / Grok / MiniMax / the Liberado-native pack are interchangeable workers.

That is the operational form of
[`positioning.md`](../spec/architecture/positioning.md) item 3: coding is third, and "good enough +
integrated", not best-in-class. It is not a new north star. The life-OS daemon remains priority 1.

## 2. Why this is worth recording

Subscriptions to several coding harnesses are already paid for. They sit behind different CLIs,
session formats, and quota counters. Liberado's native loop is one of those workers. Competing with
Codex or Claude Code on the loop is the brutal target positioning already declined.

A control plane turns that fragmentation into a resource pool:

> Use whichever coding agent you already pay for. Liberado schedules, supervises, retries, routes,
> reviews, and keeps continuity across all of them.

The hard part is continuity across harness boundaries. Do not preserve "the same chat". Each
harness has a different session format, hidden state, tool semantics, and persistence. The unit of
continuity is an **explicit task record Liberado owns**. Repo state is authoritative. The agent
conversation is disposable execution state.

That also covers crash, quota reset, provider death, and a mid-task switch from Grok to Codex:
the branch, commits, tests, and ledger survive.

## 3. Frozen seam

A coding worker is not a `Provider`. It is not `ToolRuntime`. It is not the comparison
`HarnessAdapter` (`preflight` / `launch` / `run`). Those stay where they are.

The production seam is a small worker port trait:

```rust
pub trait WorkerPort {
    /// Identifier for this worker type (e.g., "liberado-native", "codex-cli", "claude-code").
    fn id(&self) -> &str;

    /// Launch a worker on a task in the specified workspace worktree.
    fn start(&self, req: WorkerRunRequest) -> Result<RunHandle, WorkerError>;

    /// Resume a worker following a new task event (e.g. CI failure, review rejection).
    fn resume(&self, handle: &RunHandle, event: &TaskEvent) -> Result<RunHandle, WorkerError>;

    /// Poll the current status of an in-flight run.
    fn status(&self, handle: &RunHandle) -> Result<WorkerStatus, WorkerError>;

    /// Cancel a running execution.
    fn cancel(&self, handle: &RunHandle) -> Result<(), WorkerError>;

    /// Collect the final result once status is terminal (Completed or Failed).
    fn collect(&self, handle: &RunHandle) -> Result<WorkerRunResult, WorkerError>;
}
```

`resume` does **not** mean "append a message to the same LLM conversation".

- For Codex it might resume a session ID if the CLI supports it.
- For Claude Code it might invoke that product's continuation mechanism.
- For a CLI with no stable session API it starts another subprocess in the same worktree with a
  continuation prompt built from the task record.

Liberado should not care which of those happened, only that the adapter returned a `RunHandle` and
later a `RunResult`.

### 3.1 Data contracts

`WorkerStatus` represents the execution lifecycle:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerStatus {
    Running,
    Waiting,
    Completed,
    Failed,
}
```

`RunResult` is structured. Do not parse prose such as "I think everything is fixed."

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerRunResult {
    pub status: WorkerStatus,
    pub summary: String,
    pub commits: Vec<String>,
    pub files_changed: Vec<String>,
    pub tests_run: u32,
    pub tests_passed: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocking_issue: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recommended_next_action: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_session_id: Option<String>,
}
```

Derive commits, files, and test results from Git and the test runner. Trust the model for summary
and recommended next action only.

This port is **not** a kernel contract today. Do not add it to
[`contracts.md`](../spec/architecture/contracts.md) until one adapter does one real job. Do not put
it in `crates/executor` or `crates/orchestrator`. Place it in `crates/coder-core` as pack contracts.

## 4. Task record & durable ledger

Liberado owns a durable task. The unit of continuity is the append-only ledger of events.

### 4.1 Storage layout

Each task is stored under `.liberado/tasks/<task_id>/`:

```text
.liberado/tasks/<task_id>/
├── ledger.jsonl    # append-only event stream (authoritative)
└── task.json       # cached projection of current TaskRecord (derived)
```

### 4.2 Task record projection

The task state is deterministically folded from the event history:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRecord {
    pub task_id: String,
    pub repo: Option<String>,
    pub worktree: String,
    pub branch: String,
    pub base_ref: String,
    pub objective: String,
    pub acceptance_criteria: Vec<String>,
    pub status: TaskStatus,
    pub prior_worker: Option<String>,
    pub external_session_id: Option<String>,
    pub current_diagnosis: Option<String>,
    pub commits: Vec<String>,
    pub files_changed: Vec<String>,
    pub failures: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
```

`TaskStatus` lifecycle:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Created,
    Running,
    NeedsReview,
    Repairing,
    Completed,
    Blocked,
    Escalated,
    Failed,
}
```

Distinguish **task identity** from **worker-run identity**:

```text
Task 341
  Run 1: Codex
  Run 2: Codex continuation
  Run 3: MiniMax recovery
  Run 4: Claude reviewer
```

Retries, escalation, and later model comparison stay clean if those ids never collapse.

### 4.3 Append-only event history

Events record state transitions:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TaskEventKind {
    TaskCreated {
        objective: String,
        acceptance_criteria: Vec<String>,
        branch: String,
        base_ref: String,
    },
    WorkerStarted {
        run_id: String,
        worker_id: String,
        resumed_session_id: Option<String>,
    },
    CommitProduced {
        commit_sha: String,
        message: String,
        files_changed: Vec<String>,
    },
    TestsPassed {
        tests_run: u32,
    },
    PullRequestOpened {
        pr_number: u64,
        url: String,
    },
    CiFailed {
        run_id: Option<String>,
        failures: Vec<String>,
        failure_log_excerpt: Option<String>,
    },
    WorkerResumed {
        run_id: String,
        worker_id: String,
        reason: String,
    },
    ReviewRejected {
        reviewer: String,
        round: usize,
        diagnosis: String,
    },
    ReviewApproved {
        reviewer: String,
        round: usize,
    },
    CiPassed,
    Escalated {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskEvent {
    pub event_id: String,
    pub task_id: String,
    pub timestamp: DateTime<Utc>,
    pub payload: TaskEventKind,
}
```

Orchestration logic becomes a function of those events, not of chat replay.

Every worker gets a dedicated worktree and branch. Continuity must never depend on an agent
remembering what it did.

## 5. Continuity and context reconstruction

When a worker crashes, hits a quota ceiling, or cannot resume conversational chat state, Liberado
synthesizes continuation context from the task record and repo state.

### 5.1 Continuation prompt format

The continuation prompt synthesizes:

1. **Objective and acceptance bar**: frozen from `TaskCreated`.
2. **Repository context**: branch name, base commit SHA, and current `git diff --stat`.
3. **Failure report**: exact test failure identities and capped excerpt from `CiFailed`.
4. **Diagnosis**: instructions or review findings from `ReviewRejected`, if present.
5. **Scope boundaries**: explicit instructions to stay inside the task scope.

Example reconstructed prompt:

```markdown
You are continuing work on task: `liberado/task-341`.

## Objective
Implement directory enumeration in TurboVault.

## Acceptance criteria
- Enumeration returns relative paths within the vault root.
- Hidden paths (prefixed with `.`) are ignored by default.
- Unit tests pass with `cargo test -p turbovault`.

## Worktree state
- Branch: `liberado/task-341`
- Base commit: `76bcb640`
- Existing commits on branch:
  - `a1b2c3d`: Add recursive directory walk

## CI Failure Details
Tests failed in CI:
- `turbovault::tests::test_hidden_dir_ignored`

Log excerpt:
```text
running 1 test
test tests::test_hidden_dir_ignored ... FAILED
failures:
    tests::test_hidden_dir_ignored: assertion failed: !res.contains(".git")
```

## Instructions
1. Reproduce the failing test locally before modifying code.
2. Fix the defect without broadening scope or refactoring unrelated files.
3. Verify `cargo test -p turbovault` passes.
4. Commit your changes with a clear message.
```

## 6. Retry policy

Do not feed every CI error into an agent forever.

```text
CI failure
  → same worker gets one repair attempt
  → fails again
  → fresh reviewer diagnoses
  → original worker gets the diagnosis
  → still fails
  → escalate / human review
```

Shepherd already drives a PR to ready-or-blocked. Do not build a second retry loop beside it.
Extend shepherd's policy, or make shepherd a consumer of this port. Do not create a fourth engine.
A `/loop` remains a scheduler for ordinary goals
([`loops-plan.md`](loops-plan.md)).

A router may pick a backend from leftover quota:

```text
Codex quota available     → Codex CLI
MiniMax subscription left → MiniMax-backed worker
Grok allowance left       → Grok harness
cheap API task            → DeepSeek through the native pack
all pools exhausted       → defer maintenance
critical CI break         → pay API regardless
```

That router is later work. The first slice does not need it.

## 7. What already exists

Do not rebuild the spine.

| Piece | Where | Role here |
|---|---|---|
| Isolated worktrees | `coder-sandbox`, dispatch-pack | dedicated branch per worker |
| Comparison launch boundary | `HarnessAdapter` in `harness-eval` | measurement only; not this port |
| Durable jobs, journals, one-at-a-time paid runs | `liberado coder compare` | evidence path (C3), not production tasks |
| Drive a PR to ready-or-blocked | `liberado shepherd` | retry / escalate host |
| Traces | coder traces, MVL, execution log | run record, disposable vs the ledger |
| Goal sessions | `session` hub + coding pack | native worker, one backend |
| Cadence maintenance (idea) | [`cadence-triggered-maintenance-agents.md`](cadence-triggered-maintenance-agents.md) | a later *job* on this plane, also not scheduled |

The comparison adapter and the production worker port share a shape (start a harness in a
worktree, collect a result). They do not share a type. Comparison policy (native first-pass, no
repair, alternate order) is the opposite of production policy (repair, resume, escalate).

## 8. What is missing

- A task ledger that outlives a session.
- Production use of an external harness as a worker.
- Resume across harnesses from the task record when the original session is gone.
- Normalized `RunResult` independent of harness prose.
- Subscription-aware routing.

That is the whole gap.

## 9. First slice: The CI-repair ledger

One worker, one job, one ledger. Not a framework.

Suggested job: `CiFailed` on a shepherded PR → resume the same harness if a session id exists,
else start a fresh worker in the same worktree with the task record (objective, branch, failing
test, acceptance bar).

Suggested first external worker: whichever CLI is already paid for and can run unattended on the
homelab. Native Liberado remains a worker on the same port.

Acceptance for that slice:

- A task record survives the worker process (`ledger.jsonl` persists on disk).
- A second worker, possibly a different harness, can continue from that record and the git branch
  without the original chat.
- New code does not land in `crates/executor/src/lib.rs` or `crates/orchestrator/src/lib.rs`.
- No new crate until that one adapter does that one job.
- New functions stay under the new-function CRAP ceiling of 30.
- External harness writes are confined to the dedicated worktree. Liberado still owns merge / PR
  policy. The capability/zone model does not pretend to contain a foreign CLI's writes.

## 10. Implementation steps

1. **Step 1: Domain contracts and serialization (`crates/coder-core`)**
   Define `TaskRecord`, `TaskStatus`, `TaskEvent`, `TaskEventKind`, `WorkerPort`, `RunHandle`,
   `WorkerStatus`, and `WorkerRunResult`. Implement `TaskLedger` event folding and JSONL persistence.
2. **Step 2: Continuation context builder (`crates/coder-core`)**
   Implement `ContinuationContextBuilder` to turn a `TaskRecord` + CI failure excerpts into a
   normalized continuation prompt.
3. **Step 3: Worker adapter implementations**
   Implement `NativeWorkerAdapter` (delegating to the Liberado coding pack) and external adapters.
   - For harnesses that expose ACP (Agent Control Protocol) servers (e.g. Gemini `--acp`, Copilot ACP,
     Cursor ACP), we can adapt orchestration infrastructure from Paseo
     ([`https://github.com/getpaseo/paseo`](https://github.com/getpaseo/paseo), available locally under `paseo/`),
     which already drives multi-agent ACP sessions over stdio JSON-RPC.
   - For CLI-only harnesses, spawn subprocesses in the worktree passing the synthesized continuation prompt.
   - **Test harnesses one at a time on live setups**: Each harness must be tested individually on the
     actual harness process to verify that session continuation, file mutations, and failure handling
     work reliably before enabling in automated routing.
4. **Step 4: Shepherd integration**
   Wire `shepherd_cmd.rs` to write task events to the ledger and invoke `WorkerPort` for kickbacks.

## 11. Constraints (why this stays a draft)

From [`research/bob-martin-critique.md`](research/bob-martin-critique.md) and the layer rules:

- Do not add a 54th crate to encode confusion as structure.
- Do not grow `run_loop` / `run_with_turn_budget` to host this.
- Do not treat "substrate" as the product. The product sentence for this slice is "Liberado
  operates coding agents", and only for coding, and only after C3.
- Do not let this eat priority 1 (daemon) or priority 2 (lean chat).

Containment is the design hole in the source conversation. A foreign harness writing a worktree
sits outside `CapabilitySet` and zone grants. First-slice mitigation is isolation (worktree +
shepherd merge policy), not a fiction that Liberado sandboxed Codex.

Resume across harnesses reconstructs **task state**, not mid-turn tool state. That is enough for
CI-fail-continue. It is not enough to recover a crashed in-flight tool call. Do not claim otherwise.

## 12. Explicitly not this

- Making the native coding agent dramatically smarter.
- A universal LLM API that every harness must speak.
- Peer-to-peer agent coordination (already rejected;
  [`research/agent_pools_research_results.md`](research/agent_pools_research_results.md)).
- Replacing shepherd, compare, or the session hub.
- Scheduling this ahead of C3, C5, or the life-OS inbox work.

## 13. When to promote

Promote this file from `draft` to `active` and add a backlog row only after:

1. C3 is published.
2. The table shows a reason to run an external harness as a production worker, not only as a
   benchmark.
3. The first slice in §9 is small enough for one PR.

Until then, agents must not take implementation work from this document.
