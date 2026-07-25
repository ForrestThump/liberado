# Liberado — Development Workflow & Working Process

**Audience**: a fresh planning/architectural agent (or human) picking up this project cold, with no
memory of prior sessions, who needs to independently research, plan, delegate to subagents, implement,
and ship work at the same quality bar this project has been held to. `docs/contributing/agents.md`
covers *build/run mechanics*; `docs/architecture/*.md` covers *what's built*; this doc covers *how work
here actually gets done* — the process, judgment calls, and conventions that produced the work recorded
in `docs/roadmap/`. Read this before touching code on a new branch.

**Origin**: written 2026-07-02, distilled from a single long session that shipped two safety fixes
(MCP connection isolation, runtime-level tool gating), a three-tier hygiene pass (crate coupling,
duplication, dead code — 12 items), and a hardening audit (proposal-integrity gaps) using the exact
workflow described below. Concrete before/after examples from that session are used throughout as
calibration — not because they're special, but because they're the most recent evidence this process
actually works.

---

## The governing philosophy

Three principles from `docs/architecture/overview.md` shape every decision here, and are worth
internalizing before anything else:

1. **Safety is engineered, not prompted.** The LLM proposes; deterministic code disposes, only ever
   toward *less* autonomy. When you find a safety mechanism that doesn't fully close a gap (see the
   proposal-integrity work in `docs/roadmap/archive/hardening-audit-2026-07-02.md`), **say so explicitly rather
   than shipping something that looks like a fix but isn't.** A partial mitigation described honestly is
   more valuable than a full mitigation oversold.
2. **Capability only narrows, never widens**, down any delegation chain (`CapabilitySet::narrow`,
   Decision 4). Any new code that hands authority to a subagent, a tool call, or a spawned process should
   be checked against this invariant on sight.
3. **Numbered "Decision N" ledger.** Load-bearing architectural choices are tracked in
   `docs/specs/liberado-architecture-decisions.md`, referenced by number throughout the codebase's doc
   comments (e.g. "Decision 4 narrowing," "Decision 11 proposal loop," "Decision 18 incremental mesh").
   When you make a comparably load-bearing choice, add it to that ledger rather than letting the
   reasoning live only in a commit message or a chat transcript that will eventually compact away.

---

## The development loop

This is the actual sequence used this session, generalized. Not every task needs every step — a
one-line typo fix doesn't need a research phase — but for anything touching more than one file or
involving a real design choice, this is the order that's worked:

### 1. Research before design — verify, don't assume

Before proposing a fix, confirm the premise against the actual code, not against what a doc, a prior
summary, or your own intuition claims. This session's clearest example: an audit finding said
`liberado-theme`/`liberado-markdown`/`liberado-commands` were "three tiny crates, always used together" —
a plausible-sounding claim that turned out **false on both counts** the moment `wc -l` and each
consumer's actual `Cargo.toml` were checked directly (`docs/roadmap/archive/hygiene-audit-2026-07-02.md`, item
9). The merge was dropped, and a more valuable, real finding (webui bypassing `liberado-markdown` for its
own hand-rolled renderer) surfaced *while checking the premise*. **A finding that turns out wrong is a
success of the process, not a wasted step — say so and change course, don't push through to save face.**

For broad, open-ended investigation (an area of the codebase you don't already know well, or a question
with an uncertain answer), delegate to the `Explore` subagent type rather than grepping around yourself
turn by turn — see "Delegating to subagents" below. For narrow, known-scope lookups (you already know
which file and roughly what you're checking), just `Read`/`Grep` directly; spawning an agent for a
single-file check is waste.

### 2. Plan Mode for anything non-trivial

Use Plan Mode (`EnterPlanMode`/`ExitPlanMode`) for any change that's multi-file, involves an
architectural decision (which crate should own this trait? how should this data flow?), or where you'd
otherwise be guessing at the user's intent. Skip it for single-line fixes or changes with only one
reasonable shape.

**What a good plan looks like** (see any of this session's plan files, or the shape of
`docs/roadmap/archive/hardening-audit-2026-07-02.md`'s scope-decision section for a written example): a Context
section that explains *why*, not just *what* — including any premise you had to correct along the way —
followed by a concrete design with exact file paths, and a Verification section that names the actual
commands to run. **Write the plan as if the reader will judge whether you actually understood the
problem, not just whether you produced a checklist.**

If a design choice turns out to be a genuine fork with no clearly-better answer (this session hit one
mid-plan: whether a security fix's residual risk should be silently accepted, or explicitly documented as
a known limitation) — use `AskUserQuestion` rather than picking silently. Don't use it to ask "does this
plan look OK?" — that's what presenting the plan for approval is for.

### 3. Implementation discipline

- **Move code, don't duplicate it, when relocating for architectural reasons.** When `RuntimeFactory`
  moved from `liberado-orchestrator` to `liberado-executor` to break a near-circular crate dependency
  (`docs/roadmap/archive/hygiene-audit-2026-07-02.md`, item 6), every call site was updated in the same pass, not
  left as a re-export shim indefinitely (a re-export was used *temporarily* in one case —
  `liberado-bootstrap` re-exporting `liberado-config`'s surface — specifically because dozens of existing
  call sites had no reason to change and re-exporting was the correct, deliberate choice there, not a
  shortcut).
- **Check what actually depends on what before assuming a dependency is safe to add or remove.** `cargo
  tree -p <crate>` is cheap and catches wrong assumptions before they're baked into a plan (used
  repeatedly this session to confirm a dependency edge was actually removed, not just apparently removed
  because the code still happened to compile via a transitive path).
- **When splitting or extracting a crate, keep the public API stable for consumers who have no reason to
  change.** The `liberado-config` extraction from `liberado-bootstrap` required zero changes in
  `liberado-server`/`liberado-cli` because the split re-exported the full original surface — only the one
  consumer that had a *reason* to change (`liberado-mcp-forge`, which only needed the light half) was
  touched.

### 4. Testing discipline

- **`cargo build --workspace` is not enough.** It does not compile `#[cfg(test)]` code. This session hit
  real breakage more than once where trimming an "unused" top-level import broke a test module that only
  used it via `use super::*` — caught only because `cargo test --workspace --no-run` was run as an
  explicit, separate step before trusting a build was clean. **Always run both**, in that order, before
  claiming a change is verified.
- **Then run the full suite** (`cargo test --workspace`) and read the actual pass/fail counts, not just
  "no errors printed." This session tracked the exact suite count (51, then 52 after a new crate) turn
  over turn specifically to notice if anything silently stopped running.
- **`cargo tree` for structural claims.** If a change's whole point is "crate X no longer depends on
  crate Y," don't just trust that it compiles — grep `cargo tree -p X` for `Y` and confirm zero matches.
  Compiling successfully doesn't prove an edge is gone; it proves the graph is still acyclic.
- **Live smoke-test when feasible, and say so explicitly when it isn't.** The MCP connection-isolation
  fix was verified by actually re-enabling a known-broken MCP server and watching the daemon boot with
  partial tool availability instead of trusting the unit tests alone. When a live check genuinely isn't
  feasible (e.g. no way to drive a real model through a specific adaptive-call scenario on demand,
  or the operator is traveling without easy access to test a browser-facing feature), **say so plainly in
  the plan and lean on integration tests as the primary evidence** rather than silently skipping
  verification and not mentioning it.

### 5. Documentation discipline

- **Record findings before fixing them**, in `docs/roadmap/`, especially for audit-shaped work (a broad
  investigation producing a prioritized backlog). This project's convention: one doc per audit pass,
  named `<topic>-audit-<date>.md`, with a Purpose/Method header, findings organized by severity/tier with
  file:line references and an explicit verdict per item (real gap, or confirmed-fine-with-reasoning), and
  a "Recommended sequencing" close. See `docs/roadmap/archive/hygiene-audit-2026-07-02.md` and
  `docs/roadmap/archive/hardening-audit-2026-07-02.md` for the concrete shape. **This matters beyond the current
  conversation**: it's what lets a differently-scoped session (or a different agent entirely) resume the
  backlog without re-deriving it, and it survives context compaction that a chat transcript doesn't.
- **Cross-link related docs both directions.** When this session's work re-confirmed an already-recorded
  finding in `crate-modularity-audit.md`, both docs were updated with a link to the other — not just the
  new one.
- **Fix stale status markers when you find them, even if that's not what you set out to do.**
  `docs/roadmap/current.md` had a "Landed" bullet describing runtime tool gating as an open gap after it
  had actually shipped earlier the same session — a one-paragraph courtesy fix, done in passing, not
  deferred as out-of-scope.
- **Don't retroactively pad or invent numbers.** A commit message in this project once cited "194 tests"
  as a total that had not actually been counted — noticed after the fact, and deliberately *not* silently
  amended (amending without being asked violates the git discipline below); the lesson carried forward
  since: only cite a count you've actually run and read.

### 6. Git discipline

- **Never amend a commit** unless explicitly asked. Create new commits.
- **Stage explicit file lists**, not `git add -A`/`-u`, so an unrelated in-progress change on the branch
  (this project has had genuine WIP sitting alongside hardening work — see the `ui-polish` branch's
  uncommitted webui component work) never gets swept into an unrelated commit by accident.
- **Split commits along review boundaries when it helps a reader**, not mechanically. Documentation and
  the code it describes were sometimes committed together (when they're one cohesive change) and
  sometimes separately (when "record the findings" and "fix the findings" are two distinct, individually
  reviewable steps — see how the hygiene and hardening audits each got their own docs-only commit before
  the corresponding fix commit).
- **Write commit messages that explain why, not just what** — the diff already shows what changed.
- **Never push, and never run destructive operations** (`reset --hard`, force-push, `clean -f`) without
  being explicitly asked, regardless of how confident you are.

---

## Delegating to subagents

This session's audits (hygiene, hardening) each used **3 parallel Explore-type subagents**, one per
distinct angle, rather than one broad agent or a sequence of narrow ones. That shape earns its cost when:

- The investigation spans multiple, genuinely independent axes (e.g. "crate coupling," "code
  duplication," "function cohesion/dead code" — three different lenses over the same codebase, each
  producing non-overlapping findings).
- You don't yet know what you're going to find, so a single agent following one thread would miss the
  other two entirely.

**What made these prompts effective**, worth repeating as a pattern:

- **Full self-contained context.** A subagent has no memory of this conversation. State what's already
  known (file paths, prior findings, the specific architecture already confirmed), not just the question
  — otherwise it re-derives basics at the cost of the actual investigation.
- **A specific, narrow angle per agent**, not "look for problems." Three agents each told "look for
  problems" would produce three overlapping, shallow reports; three agents each told exactly which axis
  to check (coupling / duplication / cohesion) produced three complementary, deep ones.
- **Demand concrete evidence, not summaries.** Every research prompt this session explicitly asked for
  file:line references and an explicit verdict (real finding vs. confirmed-fine), which is what made
  synthesizing three reports into one prioritized backlog fast — vague prose would have needed a second
  pass just to extract actionable items.
- **A word budget** (this session used ~600-700 words per report). Without one, reports balloon and the
  signal-to-noise ratio drops when synthesizing three of them at once.

For implementation work (not research), this session did **not** delegate — plan-approved changes were
implemented directly, turn by turn, with the same build/test discipline as above applied after every
meaningful edit rather than batched at the end. That was a deliberate choice for this project's current
size (small enough that direct implementation with tight verification loops is faster and more reliable
than delegating chunks to fresh agents and re-verifying their output afterward) — reconsider it if the
codebase or the task grows past the point where one agent can hold the relevant context.

---

## Where to find things

Don't duplicate what already exists elsewhere — this doc is process, not architecture.

| Question | Where |
|---|---|
| What is this system, at a high level? | `docs/architecture/overview.md` |
| Why does it exist / how does it compare to alternatives? | `docs/architecture/positioning.md` |
| The seam/modularity plan | `docs/architecture/modularity.md` |
| How do I build/run/configure it? | `docs/contributing/agents.md` |
| What's the phased roadmap, what's landed, what's next? | `docs/roadmap/current.md` |
| Why was decision N made? | `docs/specs/liberado-architecture-decisions.md` |
| Per-crate design detail | `crates/<name>/ARCHITECTURE.md` (each crate has one) |
| The chat/SSE API contract | `docs/reference/api.md` |
| Past hygiene/hardening audit findings (including deferred items) | `docs/roadmap/*-audit-*.md` |
| Point-in-time state snapshots from past sessions | `docs/ideas/archive/handoff.md` (snapshot, not a
  process doc — expect it to be stale; this file you're reading now is the durable one) |

**A note on staleness**: `docs/ideas/archive/handoff.md` and parts of `docs/architecture/overview.md`'s "Current
status" section were found to be out of date during this session's own work (describing phases as
not-yet-done that had, in fact, shipped). That's expected drift for point-in-time docs — treat any
"current status"-shaped doc as a snapshot to verify against the actual code, not a ground truth, and fix
what you find stale in passing per the documentation-discipline section above.
