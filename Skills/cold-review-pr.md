# Cold Review PR Flow

## When to use

- Reviewing an open PR before merge (outer agent / human)
- Understanding the **in-product** cold-review stage (backlog 0.8 / Layer B)

## Product stage (in Liberado)

The coding pack owns the policy in `liberado_coder_agent::cold_review`:

```text
build → verify → cold review (diff only) → filter (cite-to-keep)
      → at most one fix round → re-verify → ready for human | escalate
```

| Hard rule | Entry |
|---|---|
| Cold reviewer sees **diff (+ optional file excerpts)** only — not goal narrative or tool trace | `build_cold_review_request` |
| Taste / standards | `prompts/coder/cold-pr-reviewer.md` (disk override + baked) |
| Cite path+location to retain a finding | `filter_findings` |
| At most **one** automatic fix round | `MAX_FIX_ROUNDS`, `decide_after_filter` / `decide_after_fix_round` |
| Ready for human only after **post-review re-verify** | `ready_for_human` |

Do **not** invent a second ad-hoc review script: retune `cold-pr-reviewer.md`.

## Operator / outer-agent flow

### 1. Checkout and diff

```bash
gh pr view <N> --json title,headRefName,additions,deletions,files
git fetch origin <branch>
git checkout <branch>
git diff main..HEAD
```

### 2. Cold-start review

Launch a subagent with the **diff only**. Tell it:

- It is a cold reviewer with NO authoring-run context
- Identify real issues: bugs, security holes, missing edge cases, design flaws
- Ignore style pedantry
- Return issues with severity (high/medium/low), **path**, and **location**
- Do NOT fix anything — just report

Prefer the same standards as `prompts/coder/cold-pr-reviewer.md`.

### 3. Filter (cite-to-keep)

For each issue:

1. Read the actual code at the cited locations
2. Keep only findings that are **real** and **code-grounded**
3. Drop uncited or hallucinated claims with a reason

### 4. One fix round

- High/medium retained findings: one fix pass on the same branch
- If still red after re-verify: **stop** and leave residual for a human — no thrash

### 5. Ready for human

Mark ready only after machine re-check passes post-review (not when the review goal *starts*).

## Key principle

The cold reviewer has zero authoring context — it will find both real bugs and false positives.
The filter that requires a code citation is what separates them; the one-fix-round cap is what
prevents unbounded auto-fix loops.
