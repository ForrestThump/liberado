# Verifiers & completeness gates — design sketch

**Status**: design sketch + **v1 coding-pack implementation** (2026-07-10).  
Shared crate extraction deferred; DTOs live in `liberado-coder-core::{verify,intake}`, pipeline in
`liberado-coder-agent::verify_pipeline`.  
**Update 2026-07-11 (audit)**: the config-layering pressure on these DTOs is resolved — not by
extraction but by inversion: `config-loader` now carries `[tuning.coder]` as an opaque
`toml::Value` and `liberado_coder_core::CoderTuning::from_value` parses/validates it, so the
config stack no longer depends on the pack. Extraction into `liberado-verify` stays a
second-domain decision per §7. See [modularity.md](modularity.md)'s extraction-trigger note.  
**Related**: [`agentic-loops.md`](agentic-loops.md), [`coder-eval-curriculum.md`](../../future-work/coder-eval-curriculum.md),
dispatcher `Clarify` / `success_criteria` in `liberado-common`,
**project-level ship preflight** (complementary): [`self-pr-quality-roadmap.md`](../../future-work/self-pr-quality-roadmap.md#generic-preflight-gate).

This document sketches **schema and trait boundaries** for harness-owned success checks: the “CI in
the loop” idea — customizable criteria, force repair until green **or** hard stop, without locking
the kernel to Rust or even to coding.

**Attempt verifiers vs preflight:** verifiers here are **in-loop** checks attached to a frozen
contract (often small and fast). **Preflight** is the **project ship bar** before ready/PR — ideally
CI-equivalent, config-driven, pack-callable, not hard-coded cargo in the coding pack. See the
self-PR roadmap section linked above.

**Biggest product gap after the verifier machinery:** *where do the checks come from?* A vague human
writeup is not a gate. Section 3 defines **criteria intake** — a structured planning session that
turns intent into frozen verifier specs (and clarifiers), before the worker is allowed to run.

---

## 1. Goals and non-goals

### Goals

1. **Harness owns truth.** Models propose done; deterministic (or config-bound) verifiers dispose.
2. **Config-customizable.** Success checks are lists of named checks, not hardcoded `cargo test`.
3. **Domain-agnostic kernel.** Same traits for coding, HTTP probes, vault artifacts, scripts, etc.
4. **Repair loop.** Failed checks produce structured feedback for another attempt within budget.
5. **Named terminals.** Never hang: success, validation failed, budget exhausted, blocked, policy denied.
6. **Layered.** Structural → process (commands) → optional model critic. Critic never overrides hard fail.
7. **Criteria are deliberate.** Gates are **authored or intake-approved**, not invented by the worker
   that will be graded against them.

### Non-goals (for this sketch)

- Auto-merging prompt/config changes.
- Unbounded “until green forever.”
- Replacing `ToolRuntime` (tools are for *acting*; verifiers are for *judging*).
- AI-generated test suites as the primary gate (optional later; only if tests **execute**).
- Letting the worker silently redefine success mid-run.

---

## 2. Placement in the kernel

```
Human writeup (vague)
        │
        ▼
┌───────────────────────────────────────┐
│  CRITERIA INTAKE (planning session)   │  smart model, structured output
│  clarify? → questions to human        │
│  else → draft GoalSpec + VerifierSpec │
└───────────────────┬───────────────────┘
                    │ human freeze (or policy auto-accept)
                    ▼
            Frozen GoalContract
                    │
                    ▼
┌───────────────────────────────────────┐
│  GOAL SESSION (worker / repair)       │
│  act with tools → VerifierPipeline    │
│  fail → repair with findings          │
│  pass → optional Critic → terminal    │
└───────────────────────────────────────┘
```

```
GoalSession (after contract is frozen)
  │
  │  after worker / repair attempt
  ▼
VerifierPipeline
  │  runs ordered VerifierSpecs against a VerifyContext
  ▼
Verdict (pass | fail + structured misses)
  │
  ├─ pass → optional Critic → Succeeded / NeedsHumanReview
  └─ fail → if attempts left → Repair with feedback
            else → ValidationFailed / BudgetExhausted
```

| Layer | Responsibility |
|---|---|
| **Kernel** | `Verifier` trait, `Verdict`, `VerifyContext` ports, pipeline order, attempt accounting |
| **Domain pack** | Concrete verifiers (command runner, git diff, vault path), context builders |
| **Config** | Which checks run, timeouts, allowlists, budgets |
| **Surfaces** | Show check results as events; do not own the gate |

Coding pack today approximates this with a **single** `validation_command` and post-loop git status.
This sketch generalizes that without requiring a big-bang rename of `coder-*`.

---

## 3. Criteria provenance — the biggest gap

Verifiers only enforce what they are given. Sources of a **frozen** contract, in order of trust:

| Source | Trust | When |
|---|---|---|
| **Human / config author** | Highest | Known project profiles (`rust-strict`), hand-written task checklists |
| **Criteria intake session** | High after human freeze | Default for natural-language goals |
| **Project profile include** | High | Language/stack defaults (cargo test, npm test) composed with intake |
| **Planner during a run** | Medium | Only if output is typed, validated, and **frozen** before worker acts |
| **Worker inventing checks** | **Forbidden** for authoritative gates | Worker may *suggest* mid-run; must not own the gate |

### 3.1 Why a structured intake session

Human writeups are usually **intent**, not a test plan:

> “Add a small todo CLI with add/list and a file store.”

That is enough for a *conversation*, not for CI-in-the-loop. Missing:

- Required paths and package name  
- Exact behaviors to check (commands? substrings? tests?)  
- Stack (Rust? Node?) → which command profile  
- Out of scope (no network, no extra features)  
- Ambiguities (sync vs async API, CLI flags)

A **smart thinking model** is a good fit **before** autonomy: turn intent into a draft contract and
**targeted clarifiers**, not into free-form long chat forever.

This aligns with existing Liberado pieces:

- Dispatcher **`Clarify`** — stop and ask when confidence is low  
- Subagent **`success_criteria: Vec<String>`** — prose bar for reports  
- Proposal / human gates — freeze before irreversible authority  

Intake is the same idea specialized to **verifier specs**.

### 3.2 Intake is not the worker

| Role | May invent | May freeze gates | Uses tools to mutate workspace? |
|---|---|---|---|
| **Intake / criteria planner** | Draft criteria + questions | No (unless policy auto-accept) | Prefer **no** — read-only explore optional |
| **Human** | Edits draft | **Yes** | n/a |
| **Worker** | Implementation | **No** | Yes |
| **Repair** | Fixes against frozen findings | **No** | Yes |
| **Critic** | Soft quality issues | No | No |

If the same model family plays intake and worker, still use **different prompts, budgets, and tool
visibility** — and a **frozen artifact** between phases so the worker cannot rewrite the exam.

### 3.3 Intake outputs (typed)

```rust
/// Result of a criteria-intake turn or session.
pub enum IntakeOutcome {
    /// Need human answers before a contract can be frozen.
    NeedsClarification {
        questions: Vec<IntakeQuestion>,
        /// Partial draft so the human sees direction, not a blank page.
        partial_draft: Option<GoalContractDraft>,
    },
    /// Model believes the contract is ready for freeze (human still confirms in v1).
    ReadyForFreeze {
        draft: GoalContractDraft,
        /// Why these checks; shown in UI / trace.
        rationale: String,
    },
}

pub struct IntakeQuestion {
    pub id: String,
    /// Short question for the human.
    pub prompt: String,
    /// Optional multiple-choice to reduce free text.
    pub options: Vec<String>,
    /// What this unlocks (e.g. "selects command verifier profile").
    pub affects: String,
}

/// Draft contract — same shape as frozen contract; freeze = validate + stamp.
pub struct GoalContractDraft {
    pub description: String,              // cleaned restatement of the goal
    pub success_criteria: Vec<String>,    // prose for model + critic
    pub verifiers: Vec<VerifierSpec>,     // machine gates (see §4)
    pub out_of_scope: Vec<String>,        // explicit non-goals
    pub assumed_defaults: Vec<String>,    // what we filled in without asking
    /// Optional pack hint: "coding", "life", "research" — for tool/runtime selection.
    pub domain_hint: Option<String>,
    /// Optional profile id to merge: "rust-strict", "node-basic".
    pub verify_profile: Option<String>,
}

/// After freeze — immutable for the duration of the goal session (except human re-open).
pub struct GoalContract {
    pub id: String,
    pub draft: GoalContractDraft,
    pub frozen_at: DateTime<Utc>,
    pub frozen_by: FreezeAuthority,  // Human | PolicyAuto { rule_id }
    pub content_hash: String,        // integrity: worker cannot mutate unnoticed
}
```

**JSON schema for the intake model** (structured output / `complete_json`) should match
`IntakeOutcome` so the harness does not parse vibes.

### 3.4 Intake session protocol

Bounded, not an open-ended therapist loop:

```text
1. Load human writeup (+ optional repo/profile context, read-only).
2. Thinking model returns IntakeOutcome (JSON).
3a. NeedsClarification → surface questions in TUI/WebUI/Telegram;
    human answers → append to writeup → go to 2 (max N rounds, e.g. 3).
3b. ReadyForFreeze → show draft criteria + verifiers to human:
      [Accept] [Edit] [Reject / more questions]
4. On Accept: validate VerifierSpecs (known types, command policy, timeouts)
   → GoalContract frozen → GoalSession may start.
5. If max clarify rounds exceeded without Ready → terminal Blocked / NeedsHumanReview
   with last partial_draft attached.
```

**Config knobs:**

```toml
[goal.intake]
enabled = true
model = "deepseek/deepseek-v4-pro"   # or a "thinking" tier from topology
max_clarify_rounds = 3
# v1: always require human Accept. Later:
# auto_freeze_when = "never" | "low_risk_profile_only" | "always_with_audit"
auto_freeze_when = "never"
# Optional read-only tools for intake (list/read only — no write)
allow_readonly_explore = true
```

### 3.5 What the model is allowed to suggest

| May suggest | Must not |
|---|---|
| Prose success criteria | Authoritative gates without freeze |
| `paths_exist` / `content_contains` drafts | `network = true` commands without flagging risk |
| Profile pick (`rust-strict`) | Arbitrary shell from untrusted strings without policy validation |
| Clarifying questions with options | Silent assumptions on security-sensitive scope |
| Out-of-scope list | Expand authority (merge, deploy) into verifiers |

Harness **validates** every `VerifierSpec` against allowlists before freeze. Invalid draft → treat as
NeedsClarification or hard error to human, not auto-strip.

### 3.6 Composition with profiles

Intake often **merges** rather than invents everything:

```text
human writeup
  + verify_profile = "rust-strict"   → cargo test, clippy, fmt
  + intake structural checks         → required paths/symbols for THIS task
  = frozen pipeline
```

Profiles reduce questions (“default to cargo test for Rust crates?” → one yes/no).

### 3.7 UI / surface contract

Surfaces (TUI, WebUI, PR-factory submit form) should support:

1. Paste/type initial goal  
2. Answer structured questions (not only free chat)  
3. Review draft verifier list (edit path strings, toggle profile)  
4. Freeze → watch session events  

PR factory can run intake **once per task** before queueing coding, or require the submitter to
attach a pre-frozen contract JSON.

### 3.8 Mapping to existing Liberado actions

| Existing | Relationship |
|---|---|
| `DispatchAction::Clarify` | Same *spirit*; intake is richer (draft contract + multi-round) |
| `success_criteria` on subagent | Prose half of the contract; keep; add machine `verifiers` |
| Dispatcher | May route “complex goal” → intake first, then execute |
| Proposal flow | Optional: freeze is a form of human acceptance of *plan*, not of *diff* |

Do **not** overload the coding worker’s system prompt to “also invent acceptance tests.” That
recreates self-grading.

---

## 4. Core types (logical / Rust-shaped)

Names are provisional. First home can be `liberado-common` or a thin `liberado-verify` crate later.
Coding can keep wrappers that convert to/from `CoderRunResult`.

### 3.1 Verdict

```rust
/// Outcome of one verifier or the whole pipeline.
pub struct Verdict {
    pub status: VerdictStatus,
    /// Stable machine id for churn detection (hash of check name + failure class + excerpt).
    pub signature: Option<String>,
    /// Human/agent-readable summary (capped).
    pub summary: String,
    /// Structured misses for repair feedback (not free-form model prose only).
    pub findings: Vec<Finding>,
    /// Optional capped log (stdout/stderr, HTTP body snippet).
    pub log_excerpt: Option<String>,
}

pub enum VerdictStatus {
    Pass,
    Fail,
    /// Check could not run (missing tool, sandbox down) — distinct from "work is wrong".
    Error,
}

pub struct Finding {
    pub check_id: String,
    pub kind: FindingKind,
    pub message: String,
    /// Optional machine hint: path, HTTP status, exit code.
    pub detail: Option<serde_json::Value>,
}

pub enum FindingKind {
    MissingPath,
    ContentMismatch,
    CommandFailed,
    CommandTimeout,
    PolicyDenied,
    UnexpectedChange,
    Custom(String),
}
```

**Repair feedback** is derived from `findings`, e.g.:

```text
Completeness/validation failed:
- command:cargo-test: exit 101
  (excerpt…)
- content:src/main.rs must contain "todos.txt"
Fix these before claiming success.
```

### 3.2 VerifyContext (what a check may observe)

Domain-agnostic **ports**, not “git workspace only”:

```rust
/// Read-only observation surface for verifiers. Implementations are domain packs.
pub trait VerifyContext: Send + Sync {
    /// Logical root for path checks (workspace, vault root, empty for pure HTTP goals).
    fn root(&self) -> Option<&Path>;

    /// Run a policy-checked command; return exit + capped stdout/stderr.
    /// Kernel does not parse language-specific output.
    async fn run_command(&self, req: &CommandRequest) -> Result<CommandOutput, VerifyError>;

    /// Read a file relative to root (path-policy applied by implementation).
    async fn read_text(&self, rel: &str, max_bytes: usize) -> Result<String, VerifyError>;

    /// List relative paths matching a glob (capped).
    async fn list_paths(&self, glob: &str, max: usize) -> Result<Vec<String>, VerifyError>;

    /// Optional: domain-specific bag (correlation id, env snapshot) — keep small.
    fn meta(&self) -> &VerifyMeta;
}

pub struct CommandRequest {
    pub program: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub timeout: Duration,
    pub cwd: Option<PathBuf>, // relative to root or absolute if policy allows
    pub network: bool,        // default false; pack/sandbox enforces
}

pub struct CommandOutput {
    pub exit_code: Option<i32>,
    pub stdout: String,  // capped
    pub stderr: String,  // capped
    pub timed_out: bool,
}
```

**Coding pack:** `VerifyContext` backed by `coder-sandbox` + path policy (host or Docker).  
**Life-ops pack:** vault-relative paths + allowlisted commands, or HTTP-only context with no `root`.  
**Kernel:** never imports `git` or `cargo`.

### 3.3 Verifier trait

```rust
#[async_trait]
pub trait Verifier: Send + Sync {
    fn id(&self) -> &str;

    /// Stable kind for events/metrics: "command" | "paths" | "content" | "http" | ...
    fn kind(&self) -> &str;

    async fn verify(&self, ctx: &dyn VerifyContext) -> Verdict;
}
```

Pipeline:

```rust
pub struct VerifierPipeline {
    pub checks: Vec<Box<dyn Verifier>>,
    pub policy: PipelinePolicy,
}

pub struct PipelinePolicy {
    /// fail_fast: stop on first Fail (default true for cheap feedback).
    pub fail_fast: bool,
    /// treat Error as Fail for terminal status (default true).
    pub errors_are_failures: bool,
}

impl VerifierPipeline {
    pub async fn run(&self, ctx: &dyn VerifyContext) -> PipelineResult {
        // ordered; aggregate findings; signature for churn
    }
}

pub struct PipelineResult {
    pub overall: VerdictStatus, // Pass only if all Pass
    pub results: Vec<(String /*id*/, Verdict)>,
    pub combined_findings: Vec<Finding>,
    pub combined_signature: Option<String>,
}
```

### 3.4 Built-in verifier kinds (config → concrete)

These are **implementations**, not kernel forks:

| Kind | Spec fields | Pass when |
|---|---|---|
| `paths_exist` | `paths: [rel, …]` | each path exists under root |
| `paths_absent` | `paths` | none of these exist (or none changed — coding) |
| `content_contains` | `path`, `must_include: [str, …]` | all substrings present |
| `content_regex` | `path`, `pattern`, `must_match: bool` | regex ok |
| `command` | `program`, `args`, `env`, `timeout`, `network` | exit 0, not timed out |
| `http` *(later)* | `method`, `url`, `expect_status`, `body_contains` | response matches |
| `json_path` *(later)* | path or HTTP body + JSONPath | value equals / exists |
| `git_nonempty_diff` *(coding pack)* | optional base_ref | porcelain non-empty |
| `git_paths_changed` *(coding pack)* | `must` / `must_not` | status paths match |

**Command** is the portable “curl an API / npm test / cargo test” hammer.  
**Structural** checks catch incomplete greenfield without waiting for a slow suite.

### 3.5 Critic (model) is not a Verifier

```rust
// Separate trait — uses Provider, not VerifyContext alone.
pub trait Critic: Send + Sync {
    async fn review(&self, evidence: &CriticEvidence) -> CriticVerdict;
}

pub struct CriticEvidence {
    pub goal: String,
    pub success_criteria: Vec<String>,
    pub pipeline: PipelineResult,  // must already be Pass for hard gates
    pub artifact_summary: String,  // e.g. diff, file list — pack-provided
}
```

Order is **law**:

1. Structural verifiers  
2. Process/command verifiers  
3. Critic (optional)  
4. Terminal status  

Critic may only say `NeedsRevision` **after** hard gates pass (or may annotate draft PRs without blocking — product choice). Critic must **not** turn a failed `cargo test` into success.

---

## 5. Config schema (sketch)

Config is **data**. Domain packs interpret only their known `type` values; unknown types fail validation at load.

### 5.0 Frozen contract artifact (JSON, for task DB / session store)

```json
{
  "id": "goal_01h…",
  "description": "Build a todo CLI with add/list and file persistence.",
  "success_criteria": [
    "CLI supports add and list subcommands",
    "Items persist in todos.txt"
  ],
  "out_of_scope": ["network sync", "TUI"],
  "assumed_defaults": ["Rust 2021 binary crate"],
  "verify_profile": "rust-check",
  "verifiers": [
    { "id": "paths", "type": "paths_exist", "paths": ["Cargo.toml", "src/main.rs", "README.md"] },
    { "id": "symbols", "type": "content_contains", "path": "src/main.rs",
      "must_include": ["add", "list", "todos.txt"] },
    { "id": "check", "type": "command", "program": "cargo", "args": ["check"], "timeout_secs": 300 }
  ],
  "frozen_at": "2026-07-10T12:00:00Z",
  "frozen_by": "human",
  "content_hash": "sha256:…"
}
```

Worker and repair receive this blob (or a hash-checked load from store). They do not write it.

### 5.1 Goal-level (domain-neutral)

```toml
# Conceptual — may live under [goal], [session], or pack-specific [coder.gates]

[goal]
description = "..."
# Prose criteria for the model + critic (not machine-enforced alone)
success_criteria = [
  "CLI supports add and list",
  "items persist in todos.txt",
]

[goal.budget]
max_attempts = 3          # outer repair loops
max_turns_per_attempt = 28
wall_clock_secs = 900
# token / cost limits via existing ResourceLimit story

[[goal.verifiers]]
id = "manifest"
type = "paths_exist"
paths = ["Cargo.toml", "src/main.rs", "README.md"]

[[goal.verifiers]]
id = "cli-symbols"
type = "content_contains"
path = "src/main.rs"
must_include = ["add", "list", "todos.txt"]

[[goal.verifiers]]
id = "cargo-test"
type = "command"
program = "cargo"
args = ["test", "--all"]
timeout_secs = 300
network = false

[[goal.verifiers]]
id = "cargo-clippy"
type = "command"
program = "cargo"
args = ["clippy", "--all-targets", "--", "-D", "warnings"]
timeout_secs = 300

[[goal.verifiers]]
id = "cargo-fmt"
type = "command"
program = "cargo"
args = ["fmt", "--", "--check"]
timeout_secs = 60
```

Non-Rust example (same schema):

```toml
[[goal.verifiers]]
id = "unit"
type = "command"
program = "npm"
args = ["test"]
timeout_secs = 180

[[goal.verifiers]]
id = "health"
type = "command"
program = "curl"
args = ["-sf", "http://127.0.0.1:8080/health"]
timeout_secs = 10
network = true   # explicit
```

Life-ops sketch:

```toml
[[goal.verifiers]]
id = "note-exists"
type = "paths_exist"
paths = ["reviews/2026-07-10.md"]

[[goal.verifiers]]
id = "webhook-ok"
type = "http"
method = "GET"
url = "https://example.com/hooks/ping"
expect_status = 200
```

### 5.2 Pipeline & policy (kernel)

```toml
[goal.verify]
fail_fast = true
# When a check fails, whether to run remaining checks (more feedback vs speed)
on_fail = "stop"           # stop | continue
repair_on_fail = true
max_identical_signatures = 2   # validation churn

[goal.verify.command_policy]
# Global defaults; pack may narrow further
timeout_secs_default = 120
output_max_bytes = 65536
# allow empty = inherit sandbox policy; or explicit allow prefixes
allow_programs = ["cargo", "npm", "curl", "python", "node"]
deny_programs = ["rm", "sudo"]
```

### 5.3 Coding pack mapping (migration)

Today:

| Existing | Becomes |
|---|---|
| `validation_command: Option<CoderCommandConfig>` | one `type = "command"` entry |
| post-loop `git status` empty → NoChanges | `type = "git_nonempty_diff"` (coding pack) |
| eval `must_change` / `content_contains` | `paths_exist` / `content_contains` / `git_paths_changed` |
| `ProgressPolicy.max_attempts` | `goal.budget.max_attempts` |
| Critic role | stays role graph; runs only if pipeline Pass |

Compatibility: if only `validation_command` is set, synthesize a one-element pipeline so PR-dispatch keeps working.

### 5.4 Profiles (reuse without forking the kernel)

```toml
# config/verify-profiles/rust-strict.toml  (include from project)

[[verifiers]]
id = "test"
type = "command"
program = "cargo"
args = ["test", "--all"]

[[verifiers]]
id = "clippy"
type = "command"
program = "cargo"
args = ["clippy", "--", "-D", "warnings"]

[[verifiers]]
id = "fmt"
type = "command"
program = "cargo"
args = ["fmt", "--", "--check"]
```

```toml
# project goal
[goal]
verify_profile = "rust-strict"
# plus project-specific path/content checks
```

Profiles are **data includes**, not Rust modules.

---

## 6. Control flow

### 6.1 End-to-end (intake → work → verify)

```text
human_writeup
  → intake_session (0..max_clarify_rounds)
       NeedsClarification → human answers → intake again
       ReadyForFreeze → human Accept/Edit
  → GoalContract frozen (hash stamped)
  → goal_session(contract):
       attempt loop (below)
```

### 6.2 Attempt / repair (after freeze)

```text
attempt = 0
feedback = prior_feedback
loop:
  attempt += 1
  worker.run(contract, tools, feedback)  # sees prose criteria + that gates will run
  pipeline = verifiers.run(context)      # authoritative; from frozen contract only
  emit VerifierFinished events

  if pipeline.Pass:
    if critic_enabled:
      c = critic.review(evidence)
      if NeedsRevision and attempts remain: feedback = c.issues; continue
      if NeedsRevision and last attempt: terminal NeedsHumanReview or Failed
    terminal Succeeded
  else:
    if signature == last_signature: churn_count++
    if churn_count > max_identical_signatures: terminal ValidationFailed (churn)
    if attempt >= max_attempts: terminal ValidationFailed
    feedback = format_findings(pipeline)
    continue  # repair role optional
```

**Model-visible `validate` tool:** optional mirror of checks for mid-loop feedback.  
**Harness pipeline:** still re-runs after report; model cannot skip it by not calling the tool.  
**Re-freeze:** only human (or explicit policy) may edit verifiers mid-goal; worker never can.

### Events (for TUI / traces)

```text
intake_started
intake_questions     { questions[] }
intake_draft         { draft preview }
contract_frozen     { content_hash, verifier_ids[] }
verifier_started    { id, kind }
verifier_finished   { id, status, summary, signature }
pipeline_finished   { overall, findings_count }
repair_scheduled    { attempt, feedback_preview }
```

Coding pack can map these into `CoderEvent` until a neutral session event exists.

---

## 7. Crate / module boundaries (loose coupling)

```
liberado-common (or liberado-verify later)
  Verdict, Finding, Verifier trait, PipelinePolicy, CommandRequest (DTO)

liberado-executor
  unchanged ToolRuntime loop  (act)

coder-sandbox / mcp / future
  VerifyContext impls

coder-agent / goal-session
  owns attempt loop + pipeline invocation

config-loader
  parse VerifierSpec[], validate unknown types, profiles
```

**Dependency rule:**  
`verify` types must not depend on `coder-tools` or git.  
Coding-specific verifiers live in the coding pack and register via a **factory**:

```rust
pub trait VerifierFactory: Send + Sync {
    fn build(&self, spec: &VerifierSpec) -> Result<Box<dyn Verifier>, ConfigError>;
}

// Kernel holds Vec of factories; coding pack registers "git_*" and reuses generic "command"
```

Unknown `type` at runtime → config error at load, not silent skip.

---

## 8. Security / policy notes

1. **Commands are not free-form agent input.** Specs come from config, frozen contracts, or
   intake drafts that pass policy validation — not from the worker’s mid-run imagination.  
2. **Intake drafts are untrusted until freeze + validation** (allowlist programs, network flags).  
3. **Allowlist programs** (or sandbox image with only needed tools).  
4. **`network = false` by default**; curl healthchecks opt in; intake must flag network checks.  
5. **Timeouts + output caps** always.  
6. **Path checks** use the same containment as tools.  
7. **Secrets** never appear in `log_excerpt` events (redact env-like patterns in summaries).  
8. **content_hash** on frozen contracts so logs can prove the worker was graded against the
   human-accepted exam. **Implemented** (2026-07-13) as a real `sha256:<hex>` over the draft, plus
   `GoalContract::verify_integrity()` — which the coding pack calls **before** applying a contract
   to a run, so gates weakened after freeze are refused rather than silently built against. (It was
   briefly a `DefaultHasher` behind a `sha256-lite:` label: forgeable, and not even stable across
   Rust releases, so a stored contract could fail to verify after a toolchain bump. A hash whose job
   is integrity has to actually be one.)

---

## 9. Relationship to “AI test bench”, “model rubric”, and “intake”

| Approach | Role in this design |
|---|---|
| **Criteria intake (thinking model)** | **Authors draft gates + clarifiers**; human freezes |
| Config/command CI (`cargo test`, `npm test`, `curl`) | **Primary process verifiers** after freeze |
| Structural paths/content | **Primary completeness verifiers** after freeze |
| Model critic on diff/artifacts | **Secondary quality** after pipeline Pass |
| AI-generated tests | Optional worker task that *produces* tests; gate is still `command` that **runs** them |

Intake **proposes** the exam. CI **grades** the student. Critic **comments** after a pass.

---

## 10. Open questions (refine on paper)

1. **Where do frozen contracts live for PR-dispatch?** Task DB JSON column? Artifact path? Both?  
2. **Fail-fast vs full pipeline** default for multi-command CI (fmt+clippy+test)?  
3. **Http verifier** in kernel vs pack (TLS, redirects, auth headers)?  
4. **When to extract `liberado-verify` / intake types** vs keep in `common` until a second domain needs them?  
5. **Partial success:** if tests fail but paths exist — `PartiallySucceeded` or always `Failed` for autonomous runs?  
6. **Auto-freeze policy:** ever allow skip-human for low-risk profiles (e.g. internal eval only)?  
7. **Intake tool access:** pure no-tools vs read-only explore of the target repo?  
8. **Same model as worker vs stronger “thinking” model** for intake (topology role tiers)?  
9. **How many clarify rounds** before Blocked — 2? 3? product default?

**Lean for v1:**  
- Multi-command + paths/content pipeline in coding pack (shim single `validation_command`).  
- **Intake session** that emits typed `IntakeOutcome` JSON; **always human freeze** in product paths.  
- Profiles for language defaults to cut questions.  
- Fail = ValidationFailed for autonomous PR factory (not soft partial).  
- No worker-authored authoritative gates.  
- Extract shared crate when life-ops wants the same pipeline + intake.

---

## 11. Worked example: greenfield todo CLI

### 11.1 Human writeup

> Add a small todo CLI with add/list and a file store.

### 11.2 Intake (illustrative)

**Round 1 — NeedsClarification:**

- Language/stack? → options: Rust, Node, Python  
- Persist where? → todos.txt vs SQLite  
- Need `cargo test` or `cargo check` is enough?

**Human:** Rust, todos.txt, cargo check enough.

**Round 2 — ReadyForFreeze** draft:

- Prose criteria: CLI add/list; persist lines in todos.txt  
- Profile: `rust-check`  
- Structural: Cargo.toml, src/main.rs, README; symbols add/list/todos.txt  
- Process: `cargo check`  
- Out of scope: network, TUI  

**Human Accept** → `GoalContract` hash stamped.

### 11.3 Goal session

Worker builds → pipeline fails (e.g. missing todos.txt string) → repair feedback → pass →
Succeeded. Critic optional.

That is **intake + CI + completeness**, config/contract-owned, not Rust-in-the-kernel.

---

## 12. Summary

| Piece | Abstraction |
|---|---|
| **Where checks come from** | Human, profile, or **intake draft → freeze** |
| Judgment | `Verifier` + `Verdict` + `Finding` |
| Observation | `VerifyContext` (ports) |
| Composition | `VerifierPipeline` + ordered specs on **frozen** contract |
| Authority | harness after each attempt; model cannot override Fail or re-freeze |
| Extensibility | new `type` + factory in a pack; new context impl |
| Stop | attempts, wall clock, signature churn, clarify-round cap |
| Soft quality | separate `Critic`, after hard Pass |

**Direction:** automated gates that keep the agent working until checks pass (or budget), with
**commands as one verifier backend**, structural completeness, **criteria intake as a first-class
phase**, and **no kernel dependency on cargo/git**.

**Implementation status (coding pack):**

1. ✅ Multi-verifier pipeline (`verify_pipeline`) + `CoderRunConfig.verifiers`  
2. ✅ Intake `run_intake` / `freeze_if_ready` / `apply_to_request` + profiles (`rust-check`, …)  
3. ✅ Mock-safe test ladder before live: fixtures + `tests/mock_intake_e2e.rs` (intake→freeze→apply→pipeline),
   ignored hybrid/live scaffold in `tests/live_scaffold.rs` — see
   [`liberado-coder-agent` ARCHITECTURE](../../../crates/coder-agent/ARCHITECTURE.md#tests-escalation-ladder)  
4. ✅ PR-dispatch freeze-before-queue + liberado-loop sole path (`goal_contract` on tasks)  
5. ✅ Production verifier profiles on factory tasks (`dispatch.yaml` `verify:` + `LIBERADO_VERIFY_PROFILE` + `VALIDATE_CMD` → contract)  
6. ⏳ Product freeze UI (human edit/accept surface beyond policy auto)  
7. ⏳ TOML `[[coder.verifiers]]` end-to-end via monorepo tuning in liberado-coder-run deployments