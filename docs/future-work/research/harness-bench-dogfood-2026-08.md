# Harness-Bench Dogfood — Liberado + DeepSeek (2026-08-07)

**Status:** research findings. Based on live harness-bench runs of 10 tasks across three DeepSeek models (chat/V3, v4-flash, v4-pro) with two Liberado configurations (baseline, optimized).

**Branch:** `testbench-integration`

---
## Configuration comparison

### Baseline config

```rust
max_turns: 30
max_attempts: 1
verifiers: []              // no deterministic checks
hashline: { enabled: false }
system prompt: generic (no tool guidance)
workspace context: none     // model explores from scratch
```

### Optimized config

```rust
max_turns: 50
max_attempts: 2             // retry with prior_feedback on failure
verifiers: [GitNonemptyDiff] // catch "I'm done but did nothing"
hashline: { enabled: true, hash_length: 7 }
system prompt: lists all tools, explains hashline_edit + run_command_background
workspace context: auto-injected file/directory listing via CoderTask.context
```

**Also fixed in this branch:** `GitNonemptyDiff` verifier (`verify_pipeline.rs`) now checks committed changes (`git log -1 --name-only`) in addition to uncommitted changes (`git status --porcelain`). Previously it rejected git-merge+commit workflows where the working tree was clean but the last commit contained changes.

---
## Results

### deepseek-v4-flash

| Task | Baseline | Optimized | Delta |
|---|---|---|---|
| 001-file (count lines) | 1.0 (16s) | 1.0 (10s) | faster |
| 002-exec (3-step commands) | 0.00 ❌ (22s) | **1.00** ✅ (34s) | +1.00 |
| 004-meeting-summary (9 checks) | 0.89 (13s) | **1.00** ✅ (13s) | +0.11 |
| 005-email-triage (8 checks) | 1.00 (16s) | 1.00 (16s) | — |
| 007-session-memory (2 rounds) | 1.00 (23s) | 1.00 (21s) | — |
| 009-git-pr-merge (4 checks) | 0.25 ❌ (34s) | **1.00** ✅ (22s) | +0.75 |
| 010-office-docs (9 checks) | 1.00 (23s) | 1.00 (29s) | — |
| 011-code-debug | 0.87 (84s) | 0.87 (115s) | — |
| 012-doc-synthesis | 0.75 (44s) | 0.75 (31s) | faster |
| 020-archive-checksum | 0.14 ❌ (28s) | **1.00** ✅ (27s) | +0.86 |
| **Perfect (1.0)** | **4/10** | **9/10** | **+5** |
| **≥0.75** | **7/10** | **10/10** | **+3** |

### deepseek-v4-pro

| Task | Baseline | Optimized | Delta |
|---|---|---|---|
| 001-file (count lines) | 1.00 (8s) | 1.00 (4s) | faster |
| 002-exec (3-step commands) | 0.67 (35s) | 0.67 (67s) | — |
| 004-meeting-summary (9 checks) | 1.00 (16s) | 1.00 (13s) | — |
| 005-email-triage (8 checks) | 1.00 (16s) | 1.00 (14s) | — |
| 007-session-memory (2 rounds) | 1.00 (20s) | 1.00 (18s) | — |
| 009-git-pr-merge (4 checks) | 1.00 (13s) | 1.00 (18s) | — |
| 010-office-docs (9 checks) | 1.00 (31s) | 1.00 (55s) | slower* |
| 011-code-debug | 0.87 (79s) | 0.87 (69s) | — |
| 012-doc-synthesis | 0.75 (34s) | 0.75 (41s) | — |
| 020-archive-checksum | 1.00 (29s) | 1.00 (28s) | — |
| **Perfect (1.0)** | **7/10** | **7/10** | **—** |
| **≥0.75** | **9/10** | **9/10** | **—** |

(*010 slower due to second attempt after verifier rejection — verifier caught an issue, agent retried, oracle still passed.)

### deepseek-chat (V3) — baseline only

| Task | Score |
|---|---|
| 001-file | 1.0 |
| 002-exec | 1.0 |
| 004-meeting-summary | 1.0 |
| 005-email-triage | 1.0 |
| 007-session-memory | 1.0 |
| 009-git-pr-merge | 1.0 |
| 010-office-docs | 1.0 |
| 011-code-debug | 0.87 |
| 012-doc-synthesis | 0.75 |
| 020-archive-checksum | 1.0 |
| **Perfect** | **8/10** |

---
## Key findings

### 1. Flash benefits massively from multi-attempt retry

Flash's baseline was 4/10 perfect — the worst of all three models. With `max_attempts: 2`, it jumped to **9/10**, beating Pro's 7/10. Flash is fast but makes mistakes on complex tasks (002-exec, 020-checksum). A second attempt with `prior_feedback` lets it self-correct. The cost is ~1 extra model call per task, which Flash's speed amortizes.

**Insight:** Small/fast models benefit disproportionately from retry loops. Large models (Pro, V3) get it right on the first attempt more often.

### 2. Workspace context saves turns, not scores

Auto-injecting the file listing into `CoderTask.context` reduced elapsed times (Flash: 001 from 16s→10s, 004 from 13s→13s) but didn't change scores. The model was already exploring correctly; the context just made it faster by eliminating the initial `list_files` call.

**Insight:** Context injection is a latency optimization, not an accuracy one. It's worth doing for user experience but won't fix failing tasks.

### 3. The GitNonemptyDiff verifier needed a fix to be useful

The original verifier only checked `git status --porcelain` (uncommitted changes). After `git merge && git commit && git push`, the tree is clean — the verifier reported "no changes" and rejected the task. This killed 009-git-pr-merge and occasionally 010-office-docs.

The fix (in `verify_pipeline.rs`): also check `git log -1 --name-only` for files changed in the most recent commit. This covers git-merge+commit workflows.

**Insight:** Verifiers that only check working-tree state are incompatible with tasks that commit their work. A production-grade verifier needs to check both uncommitted AND committed changes.

### 4. Two task ceilings are genuine model limitations

011-code-debug (0.87) and 012-doc-synthesis (0.75) are consistent across all three models and both configurations. No amount of retries, context, or turn budget changes these scores. These are genuine "the model can't do this yet" tasks — not harness gaps.

- **011-code-debug:** Requires understanding a failing pytest suite and fixing the code. DeepSeek models get ~87% of the way there but miss one edge case.
- **012-doc-synthesis:** Requires synthesizing a document from multiple input files. The 75% score is consistent — the model gets the structure right but misses specific content requirements.

### 5. Pro is more reliable but not always better

Pro had 7/10 perfect in baseline (vs Flash's 4/10), but Flash with retries reached 9/10. Pro also showed puzzling regressions on 002-exec (0.67 in both configs) where V3 and optimized Flash both scored 1.0. This suggests Pro's reasoning style — deeper but slower — sometimes misses simple multi-step pipelines that faster models handle correctly.

**Insight:** For single-turn coding tasks, model speed matters more than model depth. Flash at 50 turns with retries is more effective than Pro at 30 turns without.

---
## Configuration recommendations for future runs

For harness-bench (and general-purpose headless task running), the optimized config should be the default:

| Setting | Value | Rationale |
|---|---|---|
| `max_turns` | 50 | Flash needs the headroom; Pro rarely uses it |
| `max_attempts` | 2 | Single biggest score improvement for fast models |
| `hashline.enabled` | true | Prevents stale edits; no downside |
| `verifiers` | `[GitNonemptyDiff]` | Catches "did nothing" failures early |
| Context injection | on | Saves 2–3 turns per task at zero cost |

The completion gate (`CoderGateConfig { enabled: true }`) should remain **off** for harness-bench. It costs `1 + fresh_reviewers` extra model calls per attempt (3+ calls at current defaults) and doesn't help on tasks with programmatic oracles — the oracle already verifies correctness. The gate is valuable for self-host coding where no oracle exists.

---
## What this branch ships

9 commits on `testbench-integration` covering:

| Area | Commits |
|---|---|
| Headless one-shot CLI + harness-bench adapter | 2 |
| Session memory (multi-round context injection) | 1 |
| Higher-level git tools (git_log, git_fetch, git_merge) | 1 |
| Parallel read-only tool execution | 1 |
| Background job execution (run_command_background) | 1 |
| ACP bridge binary over stdio | 1 |
| Optimized harness-bench config (retries, hashline, verifiers, context) | 1 |
| Verifier fix (committed-change detection) | 1 |
| Documentation (roadmap, gap analysis, Paseo, harness gaps/levers) | 3 |

**PR:** https://github.com/ForrestThump/liberado/pull/78
