---
kind: plan
status: active
authority: implementation
domain: coding-harness
canonical_for: coder-eval-curriculum
open_items: true
---

# Coder eval curriculum — progressive stress

**Purpose**: score and improve Liberado's coding-worker system prompt with **increasingly hard**
workspace tasks, so we do not declare victory after two easy one-file wins.

**Runner**: `liberado-heuristics-tuner` with `TUNER_LAYER=coder`.  
**Proposal only**: winners go to `prompts/coder/coder.md` / `LIBERADO_CODER_PROMPT` by human hand.

---

## Ladder

| Tier | Env | What it proves | Scenario count |
|---|---|---|---|
| **Smoke** | `TUNER_CODER_TIER=smoke` | Binary/tool loop works; real diffs, not false success | 2 |
| **Core** | `core` (default) | Multi-file + path hygiene + honest failure | 5 |
| **Stress** | `stress` | Rename, surgical edits, repair, multi-module | 10 |
| **Greenfield** | `greenfield` | Build multi-file projects from near-empty repo | 13 |

Higher tier **includes** all lower-tier scenarios (ordered smoke → core → stress → greenfield).

Focused greenfield-only (skip lower tiers already green):

```powershell
$env:TUNER_CODER_TIER = "greenfield"
$env:TUNER_CODER_SCENARIOS = "greenfield-todo-cli,greenfield-kv-store-lib,greenfield-config-service"
```

Optional: `TUNER_MAX_SCENARIOS=N` after the tier filter for a cheap slice. **Important:** if
`config/tuner.toml` sets `max_scenarios`, env must override it for a full tier run.

---

## Escalation plan (recommended)

### Always (CI) — mock curriculum

No API key. Scripted `MockProvider` + real `LiberadoLoopBackend` workspaces:

```powershell
cargo test -p liberado-heuristics-tuner --lib mock_curriculum
```

Covers **all smoke + core** scenarios (and scripted stress samples). Implementation:
`crates/heuristics-tuner/src/coder_curriculum_mock.rs`.

### Sparse live (opt-in)

| Rung | Command | When |
|---|---|---|
| Mock e2e (intake/pipeline) | `cargo test -p liberado-coder-agent --test mock_intake_e2e` | every PR |
| Live worker smoke | `... openrouter_deepseek_live_coding_smoke -- --ignored` | after harness changes |
| Live intake | `... --test live_scaffold live_intake_schema_smoke -- --ignored` | after intake changes |
| Hybrid intake→mock worker | `... live_intake_then_mock_worker -- --ignored` | after freeze path changes |
| Live tuner tier | `TUNER_LAYER=coder TUNER_CODER_TIER=smoke` + key | prompt search only |

### Historical live ladder (prompt fitness)

1. **Smoke** — live 2026-07-10: 2/2, accuracy 1.0.
2. **Core** — live 2026-07-10: 5/5, accuracy 1.0.
3. **Stress** — live 2026-07-10: 10/10, accuracy 1.0.
4. **Greenfield** — build multi-file crates from near-empty repos (todo CLI, kv lib, config modules).
5. Promote real PR-dispatch misses; multi-sample / multi-model for flake hunting.

Stress is green live — greenfield is the next complexity step (scaffold+prompt if it fails).
Mock curriculum must stay green so greenfield live failures are not false plumbing bugs.

---

## Example runs

```powershell
# Full core (override file max_scenarios if needed)
$env:TUNER_LAYER = "coder"
$env:TUNER_CODER_TIER = "core"
$env:TUNER_MAX_SCENARIOS = "20"
$env:TUNER_MAX_GENERATIONS = "2"
$env:TUNER_MUTATIONS_PER_CANDIDATE = "2"
$env:TUNER_COLD_STARTS_PER_GENERATION = "1"
$env:TUNER_CALL_BUDGET = "300"
cargo run -p liberado-heuristics-tuner --release

# Full stress
$env:TUNER_CODER_TIER = "stress"
$env:TUNER_CALL_BUDGET = "500"
$env:TUNER_MAX_GENERATIONS = "2"
cargo run -p liberado-heuristics-tuner --release
```

Artifacts: `<LIBERADO_DATA_DIR or .liberado>/tuner/<timestamp>/`:

| File | Role |
|---|---|
| `final.txt` / `generation-*.txt` | Rubric + proposed prompt text |
| `PROPOSAL.md` | Human decision summary (recommended?) |
| `proposal.json` | Machine metadata + metrics |
| `proposed/prompts/coder/coder.md` | Proposed file body only (not live-applied) |
| `pr_factory_task.json` | Optional hand-off to PR-dispatch after human review |

**Decision 14:** the tuner never writes into the repo `prompts/` tree and never opens PRs itself.

---

## Adding harder scenarios

Add to `crates/heuristics-tuner/src/coder_scenarios.rs` at the **end** of the stress block:

1. Prefer **deterministic content checks** (`content_contains`) over "any diff".
2. Include **must_not_change** for safety/distractor files.
3. Keep seed files small; scoring does not need `cargo test` unless we add a validate gate later.
4. Promote recurring PR-dispatch failures into scenarios with the same shape.

---

## Metrics that matter

| Metric | Meaning |
|---|---|
| coding accuracy | Scenario pass rate (paths + content + outcome) |
| nonempty-diff rate | Real workspace changes (false-success detector; no-op scenario legitimately lowers this) |
| unsafe path touches | Hard gate — must stay 0 for beam survival |
| outcome-match rate | Honest report vs expected Succeeded/Failed |

---

## When a scenario fails: prompt vs scaffolding

Do **not** default to “tweak the prompt forever.” Liberado’s lesson from doom loops and
`REPORT_NUDGE` is: **if the model is following incentives the harness gives it, fix the harness.**

### Triage order (cheap → expensive)

1. **Reproduce once** — same scenario, one model, read the trace / tool sequence if available.
2. **Classify the failure** (table below).
3. **Prefer code/gates when the failure is structural**; prefer **prompt (tuner) when the model had
   the tools and still chose poorly.**
4. **Promote interesting failures into permanent scenarios** so they never regress silently.

| Failure pattern | Likely layer | Fix |
|---|---|---|
| Claims success, empty diff | **Scaffold** | Already gated (`NoChanges`); tighten nudge/progress, not more prose |
| Touches `must_not_change` / secrets | **Scaffold + prompt** | Path policy / deny globs first; prompt “stay scoped” second |
| Wrong content but right files | **Prompt** (or content gate) | Tuner mutate; optional stricter `content_contains` |
| Never calls write tools (read thrash) | **Scaffold** | Progress guards, same-tool limits (already present); prompt only if nudge text is wrong |
| Correct idea, tool schema/API fails | **Tools** | Fix tool errors, schemas, caps, ambiguous-edit feedback |
| Multi-file partial (one of two paths) | **Prompt first** | If chronic → planner role or explicit multi-file checklist in prompt |
| Repair/refactor needs more turns | **Scaffold knobs** | `max_turns` / budgets before rewriting the whole protocol |
| Validation/repair loops forever | **Scaffold** | Validation churn guard, repair routing, failure signatures |
| Same miss across *all* models | **Scaffold or scenario** | Harness bug or impossible/ambiguous scenario — not prompt wording |
| One model fails, others pass | **Prompt or model tier** | Tuner + maybe model floor; not a tool redesign |
| Scenario scoring disagrees with “human would accept” | **Eval** | Fix expectation / content checks, not the agent |

### Rules of thumb

- **Gates beat vibes.** If a failure mode is “agent can claim X while reality is Y,” add a
  deterministic check. Prompts cannot own truth.
- **Tuner is for style of work** (inspect→edit→verify, scope, honesty when stuck) once tools and
  gates are adequate.
- **One failure class → one primary fix.** Don’t ship a prompt essay *and* three harness changes
  without knowing which one moved the metric.
- **Scaffold changes need a unit/mock test;** prompt changes need a tuner rubric + scenario that
  would have failed before.

### After stress failures

1. List failing scenario names from `final.txt`.
2. Bucket each row of the table above.
3. Implement scaffold/tool fixes first if any row is “structural.”
4. Re-run **only failing scenarios** (or full stress) to confirm.
5. Run a short coder-layer tuner pass so the prompt absorbs remaining behavioral misses.
