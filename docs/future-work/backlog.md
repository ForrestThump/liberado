---
kind: plan
status: active
authority: implementation
domain: product
canonical_for: implementation-backlog
open_items: true
---

# Backlog — implement in this order

This file is the only queue from which an agent should select implementation work. It contains
open work only. Code, tests, specifications, validation records, and git history describe work
that has landed.

Take the first open, unblocked item. Do not choose a later item because it is easier. Use one item
per PR. Before implementation, verify the item against current code and recent git history.

If an item is blocked only by elapsed time, another repository, or an external service, record the
exact blocker in [`current_unmerged_work.md`](current_unmerged_work.md) and take the next unblocked
item. Do not skip a code dependency.

## Implementation order

| Order | Item | Dependency |
|---:|---|---|
| **1** | **0.7 / C3 — publish the controlled cross-harness baseline** | The comparison infrastructure and ship-bar excerpt fix are present. This is a report, not another harness change. |
| **2** | **C5 — measure the completion gate** | Run after the baseline produces a non-zero finish rate. Do not change the default first. |
| **3** | **A1 — read one day of deployed token-economics data** | Measure the existing production system before changing its tool catalogue. |
| **4** | **A2 — narrow the tool catalogue** | Blocked on A1. Change only what the measurement supports. |
| **5** | **E4 — add directory enumeration in TurboVault** | External prerequisite for E2. Record the upstream commit. |
| **6** | **E5 — stop the TurboMCP SSE reconnect storm** | External reliability work; restore useful homelab diagnostics. |
| **7** | **E2 — implement the inbox layer** | E4 must land first. The design is settled in the inbox specification. |
| **8** | **C4 — finish dedicated goal-view panes** | Useful surface work, but it does not block measurement or unattended shipping. |

## Acceptance context

### 0.7 / C3 — controlled cross-harness baseline

Instrument pinned Liberado, Pi, Hermes, and Deep Agents versions. Use the same task, repository
commit, model, provider, sampling settings, and resource limits. Keep each harness's native system
prompt and tool schemas. Run repeats where cost permits.

Report:

- Ship-gate and merge-ready rate.
- Cost per accepted result.
- p50 and p95 duration where the sample permits them.
- Human repair required.
- Trace-linked failure classes.

A single comparison without Hermes is evidence, not this baseline. Do not rank harnesses or change
`max_turns` from that sample. See
[`harness-comparisons.md`](../spec/reference/harness-comparisons.md) and
[`harness-study-2026-08.md`](harness-study-2026-08.md).

### C5 — completion-gate measurement

Run the same controlled task set with `[coder.gate] enabled` off and on. Measure accepted results,
model calls, cost, and repair. The gate costs `1 + fresh_reviewers` model calls per attempt. Keep it
default-off until the result supports a change.

### A1 — deployed token-economics read

Deploy the existing instruments, wait one day, and report:

- Offered and surviving MCP counts.
- Dispatcher cache-hit rate.
- Subagent and direct-execution shares.
- Total repeated calls.

Use `liberado-cost --json`. This item is measurement only. See
[`token-economics-findings-2026-08.md`](token-economics-findings-2026-08.md).

### A2 — tool-catalogue narrowing

Change only the catalogue path that A1 identifies. Do not rewrite prompt content while moving or
narrowing blocks. Update `crates/cost/tests/journal_shape.rs` with any change to the latency journal
shape.

### E4 — TurboVault directory enumeration

The inbox layer needs a positive directory scope. In the TurboVault repository, implement one
supported path: enable the required SQL query, add a positive `path_prefix` to search, or add a
`list_notes(path)` operation. Record the upstream commit before E2 begins.

### E5 — TurboMCP SSE reconnect storm

Diagnose and stop the idle read-error, stream-ended, reconnect loop in `turbomcp_http::transport`.
The observed rate was about 50 reconnects per minute and displaced useful logs. Treat this as an
external dependency and record the upstream commit.

### E2 — inbox layer

Implement the two capture surfaces and compare-and-swap clearing defined in
[`inbox-spec.md`](../spec/inbox-spec.md). Unflagged notes belong to the schedule; `#now` notes belong
to the watcher. Reuse existing provenance and content-hash idempotency.

### C4 — dedicated goal-view panes

Add a role timeline, gate panel, and verifier panel to `crates/tui`. Gate votes already stream but
currently render in the joined pane. Follow
[`session-surface-contract.md`](../spec/architecture/session-surface-contract.md).

## Deferred, not selectable

### C6 — repository map and context selection

Focused search is sufficient today. Do not schedule an always-on repository map until measurement
shows that missing context, rather than missing files, limits accepted results. If the item reopens,
keep goal-context ranking in the kernel and source-tree or symbol-graph work in the coding pack.

## Branch and integration rule

Before creating a branch, record these fields in
[`current_unmerged_work.md`](current_unmerged_work.md):

- Base commit.
- Predecessor.
- Shared files.
- Merge order.

Stack only work that shares a dependency or integration point. Branch independent work from current
`main`. After a predecessor merges, rebase dependent branches, rerun local gates, and require fresh
GitHub CI.

## Required PR evidence

The repository PR template asks for:

1. Evidence that the backlog item is still open.
2. One executed defect mutation for each changed behavior.
3. Evidence from the real code path when it can be driven.
4. A kernel-or-pack placement decision.
5. Base, predecessor, shared-file, and merge-order details.

Run `just ci` before push. A mutation must be applied, observed to fail, and restored without using
`git checkout` on a file that can contain uncommitted work.
