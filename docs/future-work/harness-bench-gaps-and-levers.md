---
kind: finding
status: active
authority: advisory
domain: coding-harness
canonical_for: harness-bench-gaps
open_items: true
---

# Harness-Bench — Task Gaps & Performance Levers

**Status:** analysis, 2026-08-07. Based on live dogfood of 10 harness-bench tasks across deepseek-chat, v4-flash, and v4-pro.

---
## Part 1 — Missing task categories

The 106 existing harness-bench tasks skew toward single-agent file ops. Liberado's differentiators (verifiers, fan-out, checkpoints, plan mode, capability topology) have zero task coverage. Suggested additions, ordered by how well they test Liberado's strengths:

### Category A: Verifier-gated correctness

Tasks where "I think I'm done" is insufficient — the agent must pass a programmatic check.

| Task | Prompt sketch | Oracle |
|---|---|---|
| **A1-verifier-rust** | Write `src/lib.rs` with a function `add(a, b) -> i32`. Then run `cargo test`. If the test fails, fix and retry. | `cargo test` exits 0 AND `src/lib.rs` contains `fn add` |
| **A2-verifier-content** | Create `out/letter.txt` with a formal business letter containing exactly the phrases "Q3 revenue", "42% growth", and "FY2026 outlook". | Content-contains check for all 3 phrases |
| **A3-verifier-structure** | Create `out/config.yaml` with keys `server.port` (integer), `server.host` (string), `database.url` (string). | YAML parse + schema check |

**Why this matters:** Liberado is the only harness with deterministic verifiers-as-code. These tasks prove whether the verifier pipeline actually catches incomplete work — a test none of the existing 106 tasks perform.

### Category B: Multi-agent fan-out

Tasks where the model should decompose work across subagents.

| Task | Prompt sketch | Oracle |
|---|---|---|
| **B1-fanout-analysis** | Analyze `src/a.rs`, `src/b.rs`, `src/c.rs` for security issues. Produce a single `out/audit.md` combining findings from all three. | Content-contains a finding from each file, all in one document |
| **B2-fanout-refactor** | Rename the public API in `src/old_api.rs` to match the new convention in `src/new_api.rs`. Update all callers across the codebase. | No references to old API names remain; all callers compile |

**Why this matters:** Liberado's S6 fan-out (hub-spawned children + LLM merge-back) is a headline feature with zero benchmark coverage. These tasks prove parallel width actually works.

### Category C: Plan-then-build

Tasks requiring a plan artifact before code changes are allowed.

| Task | Prompt sketch | Oracle |
|---|---|---|
| **C1-plan-mode** | First write a plan to `.liberado/plan.md` listing files to touch, functions to change, and risks. Only then implement the change. If the plan is incomplete, reject and replan. | Plan file exists with all 3 sections; implementation matches plan; no files changed beyond plan scope |
| **C2-plan-drift** | Plan a 3-file refactor in `.liberado/plan.md`. The oracle will check that the agent did NOT touch files outside the plan. | Plan exists; only planned files changed; unplanned files untouched |

**Why this matters:** Plan mode presets shipped (#67/#68) but have no benchmark proving the policy actually constrains behavior.

### Category D: Session memory & continuity

Tasks requiring state to survive across sessions or interruptions.

| Task | Prompt sketch | Oracle |
|---|---|---|
| **D1-checkpoint-resume** | Start implementing a 2-file feature. Midway (after creating file 1), the session is interrupted. Resume and complete file 2 using the checkpoint. | Both files exist; file 1 content matches the mid-build checkpoint; file 2 is new |
| **D2-memory-across-restarts** | Round 1: Store 3 configuration values. Round 2: Recall and apply them to generate a config file. | Config file contains all 3 values verbatim |

**Why this matters:** S4 checkpoints (#73) and session memory (#9eb59ae) are tested in isolation but not under harness-bench's multi-round model.

### Category E: Tool-use chaining

Tasks requiring long, correct tool-call sequences.

| Task | Prompt sketch | Oracle |
|---|---|---|
| **E1-deep-chain** | Find the most recently modified `.rs` file, read it, find the first `pub fn`, write its signature to `out/signature.txt`, then write a unit test for it in `tests/`. | Signature matches; test file exists and compiles |
| **E2-git-workflow** | Create a branch `fix/typo`, fix all typos in `README.md`, commit, push to origin, create a PR description in `out/pr.md`. | Branch exists on remote; README has no typos; PR description is non-empty |

**Why this matters:** Current tasks have 2–4 step sequences. Real agentic coding involves 5–10 tool calls in a single turn. These tasks test whether the model maintains coherence across deep chains.

### Category F: Autonomy under constraint

Tasks with restricted tools or budgets.

| Task | Prompt sketch | Oracle |
|---|---|---|
| **F1-constrained-tools** | Complete the task using only read_file, write_file, and search_text. No shell commands, no git tools. | Task completed; no disallowed tool was used (check proxy trace) |
| **F2-turn-budget** | Implement a feature in 5 turns or fewer. If you exceed the budget, write `out/budget_exceeded.txt`. | Feature works OR budget_exceeded.txt exists (no silent overrun) |

**Why this matters:** Liberado's capability topology and budget system are unique. These tasks prove narrowing actually works.

### Disposition: which belong in harness-bench vs Liberado's own test suite

The line: **if it tests Liberado's implementation, it's a unit/integration test. If it tests model behavior across any harness, it's a harness-bench task.**

| Category | Liberado unit/integration tests | harness-bench tasks | Rationale |
|---|---|---|---|
| **A: Verifier-gated** | ✅ Pipeline correctness: does the gate fire? block submit_report? feed back prior_feedback? (Already covered by `completion_gate_e2e.rs`) | ✅ **A1–A3**: can the model self-correct under verifier pressure? | Pipeline is impl-specific; model behavior under verifiers is generic |
| **B: Fan-out** | ✅ Hub spawning, worktree isolation, merge protocol, concurrency limits (already covered by `fanout.rs` unit tests) | ❌ Too harness-specific — Grok uses task+wait_tasks, OpenCode uses task tool, Kimi uses AgentSwarm. No standard fan-out protocol | Each harness has incompatible subagent architecture |
| **C: Plan-then-build** | ✅ Plan mode FSM: does PathPolicy actually block writes outside `.liberado/plan.md`? Does explore mode deny write tools? | 🟡 **C1** could work as a generic "plan first" prompt; **C2** (plan drift detection) is Liberado-specific | Plan artifact location varies; policy enforcement is impl-specific |
| **D: Session memory** | ✅ Checkpoint state machine: does shadow-git snapshot/restore correctly? Does park/resume preserve session state? | ✅ **D2** (memory across rounds) — same pattern as existing 007-session-memory; **D1** (checkpoint resume) is Liberado-specific | State mechanism is impl-specific; memory behavior is generic |
| **E: Deep chains** | ❌ Not architecture-specific — the executor loop is already tested | ✅ **E1, E2**: model reasoning depth across 5–10 sequential tool calls. Tests any harness equally | Pure model capability test |
| **F: Constrained autonomy** | ✅ Capability narrowing: does PathPolicy deny unauthorized paths? Does CommandPolicy block disallowed commands? (Already covered by `coder-tools` tests) | 🟡 **F1** (no shell commands) is a generic constraint any harness can apply; **F2** (turn budget) is Liberado-specific | Narrowing mechanism is impl-specific; working within constraints is generic |

**Net: 7 harness-bench tasks (A1–A3, C1, D2, E1, E2), 5 Liberado-specific tests (B1–B2 fan-out, C2 plan-drift, D1 checkpoint-resume, F2 turn-budget).**
The Liberado-specific ones already have partial coverage in existing tests; the gaps are mostly in edge cases and live-model verification.

---
## Part 2 — Performance levers (what to tune)

Based on the benchmark results, here are concrete changes that would raise scores without changing model or tools.

### Lever 1: Multi-attempt retry with prior feedback

**Current:** `ProgressPolicy { max_attempts: 1 }` — every task is one-shot.  
**Change:** `max_attempts: 2` with `prior_feedback` from the first failure.

**Why:** 002-exec scored 0.0 (Flash) and 0.67 (Pro) — both partial failures. The task has 3 sub-steps. If attempt 1 fails step 2, the feedback "step 2 failed: expected X, got Y" would let attempt 2 fix it. Cost: 1 extra model call per failed task. Expected gain: 002, 009, 020 would all improve.

**Code location:** `crates/coder-runner/src/main.rs` — `ProgressPolicy::default()` → change `max_attempts` from 1 to 2.

### Lever 2: Turn budget per task class

**Current:** Uniform 30 turns for all tasks.  
**Change:** Scale by task complexity. Simple file ops (001) need ~5 turns. Git workflows (009) need ~15. Code debugging (011) needs ~40.

**Why:** 011-code-debug consistently scores 0.87 across all three models — the model runs out of turns before fully debugging. 012-doc-synthesis at 0.75 likely has the same issue.

**Code location:** `crates/coder-runner/src/main.rs` — `DEFAULT_MAX_TURNS` or per-task override in harness config.

### Lever 3: Auto-injected workspace context

**Current:** The model starts blind. It must `list_files`, `read_file`, `git_status` just to orient itself.  
**Change:** The runner auto-injects a workspace summary into `CoderTask.context`:

```
Workspace summary:
  Files: src/main.rs, src/lib.rs, Cargo.toml, README.md
  Git: on branch main, 3 commits, clean working tree
  Language: Rust (detected Cargo.toml)
```

**Why:** Saves 2–3 wasted turns on every task. Those turns could go toward actual work. The existing `list_files` + `git_status` + `list_symbols` tools can generate this automatically.

**Code location:** `crates/coder-runner/src/main.rs` — `run_headless()` before building the request.

### Lever 4: Task-aware system prompt

**Current:** One generic system prompt for all tasks.  
**Change:** The prompt should be aware of what's already in the workspace:

```
The workspace already contains:
  src/  (3 files)
  in/input.txt  (4 lines)
  out/  (empty)
You do NOT need to list_files to discover this. Start working immediately.
```

**Why:** The model often runs `list_files` as its first action even when the task description already tells it what files exist. This is a wasted turn. Telling it what's there eliminates exploratory tool calls.

### Lever 5: Pre-configured verifiers from task oracles

**Current:** No verifiers are configured for harness-bench runs. The model's `submit_report` is accepted at face value.  
**Change:** Auto-generate `VerifierSpec` entries from the task's oracle checks. For 001-file, that's `VerifierSpec::ContentContains { path: "out/linecount.txt", must_include: ["4"] }`. For 002-exec, three separate `ContentContains` checks.

**Why:** The model would self-correct before reaching the oracle. If it writes "5" instead of "4", the verifier catches it in-loop and the model fixes it — no need for a full retry. This is Liberado's killer feature and it's currently unused in benchmarks.

**Code location:** `crates/coder-runner/src/main.rs` — parse the task's `oracle_grade.py` and generate `VerifierSpec` entries (or add them manually to the harness config).

### Lever 6: Hashline anchoring for multi-file tasks

**Current:** `hashline: Default::default()` — hashline edit mode is off.  
**Change:** Enable hashline for tasks that edit existing files. Hashline anchors read/write operations to specific file versions, preventing the model from editing a file that changed between read and write.

**Why:** On tasks like 011-code-debug (0.87), the model reads a file, thinks about a fix, then edits it — but the edit might be stale if git operations changed the file in between. Hashline prevents this.

### Lever 7: Background build during reasoning

**Current:** The model runs `cargo test` inline, blocking for 10–30 seconds.  
**Change:** Tell the model about `run_command_background` + `check_background` in the system prompt. For Rust tasks, suggest spawning `cargo test` in the background early, then checking results later.

**Why:** On 011-code-debug (83s elapsed), half the time is waiting for builds. A background build would let the model continue investigating while tests run.

---
## Priority sequencing

Do these in order — each multiplies the previous:

| # | Lever | Effort | Impact | Risk |
|---|---|---|---|---|
| 1 | Multi-attempt retry (max_attempts: 2) | 5 min | High — fixes partial failures | Zero |
| 2 | Workspace context injection | 1 hour | Medium — saves 2–3 turns per task | Low |
| 3 | Task-aware system prompt | 30 min | Medium — eliminates redundant exploration | Low |
| 4 | Verifier specs from oracles | 2 hours | High — Liberado's killer feature, currently off | Medium (oracle parsing is fragile) |
| 5 | Turn budget scaling | 30 min | Medium — helps 011/012 | Low |
| 6 | Hashline enable | 15 min | Low — helps multi-edit tasks | Low |
| 7 | Background build guidance | 15 min | Low — helps build-heavy tasks | Low |

**Total effort: ~5 hours for levers 1–3, another 2–3 for 4.**
