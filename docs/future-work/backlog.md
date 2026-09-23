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

If an item is blocked only by another repository or an external service, record the exact blocker
in the local `current_unmerged_work.md` (repo root, never committed) and take the next unblocked
item. Do not skip a code dependency.

**CAS4 is the exception.** It is operator time on purpose. It is the current item. Do not skip it
because the clock has not finished, and do not start a later row while it is open.

## Implementation order

Landed chat-surface work (not rows): wire stamp [#274](https://github.com/ForrestThump/liberado/pull/274),
WebUI shelves [#276](https://github.com/ForrestThump/liberado/pull/276), tunable `agent_profiles`
[#277](https://github.com/ForrestThump/liberado/pull/277), example `chat-default` hat
[#278](https://github.com/ForrestThump/liberado/pull/278). Contract:
[`../spec/architecture/chat-agent-surface-mode.md`](../spec/architecture/chat-agent-surface-mode.md).

| Order | Item | Dependency |
|---:|---|---|
| **1** | **CAS4 — use the shelves and write notes** | **Current.** Operator work, not an agent implementation. Slices 1–3 are on main. Do not skip. See the acceptance section below. |
| **2** | **The first dogfood note that names one code change** | Opens when the operator points at a note. One note, one PR. Replaces the fallback rows below for that change. |
| **3** | **R1 — prove Codex-only daemon PR review and one-repository cutover** | Fallback after CAS4 closes with no code note. Slices 0–3 and remote PR acquisition are present. Run the restart, clean, blocker, synchronize, and remote-only-head cases from [`daemon-pr-review-kickoff.md`](daemon-pr-review-kickoff.md). Publish the evidence before the worker-fallback slice. |
| **4** | **0.7 / C3 — publish the controlled cross-harness baseline** | Fallback. The comparison runner is present. This is a report, not another harness change. Spec: [`cross-harness-baseline.md`](cross-harness-baseline.md). |
| **5** | **C5 — measure the completion gate** | Run after the baseline produces a non-zero finish rate. Do not change the default first. |
| **6** | **A1 — read one day of deployed token-economics data** | The logger is already at info. Read the deployed daemon. Do not change the catalogue in this item. |
| **7** | **A2 — narrow the tool catalogue** | Blocked on A1. Change only what the measurement supports. |
| **8** | **E4 — add directory enumeration in TurboVault** | External prerequisite for E2. Record the upstream commit. |
| **9** | **E5 — stop the TurboMCP SSE reconnect storm** | External reliability work; restore useful homelab diagnostics. |
| **10** | **E2 — implement the inbox layer** | E4 must land first. The design is settled in the inbox specification. |
| **11** | **C4 — finish dedicated goal-view panes** | Useful surface work. It does not block measurement or unattended shipping. The daily surface for the current bet is the WebUI. |
| **12** | **JEV1 / CAS5 — TypeSafe Jev first wedge** | Blocked on a finished CAS4 week. Spec: [`../spec/architecture/jev-integration.md`](../spec/architecture/jev-integration.md). Confidence-gated advisor only (belong? / which agent? / optional soft tool hints). Dispatcher remains sole grantor. |

Jev stays blocked until CAS4 has actually been used for at least a week and `agent_profiles` has
been tuned, or explicitly left as-is with a short note. Its first wedge reads against an
addressable `agent_profiles` set and Agent shelves, not against `goal.is_some()`.

## CAS4 — use the system

This row closes in one of two ways:

1. The operator names a headache, bug, or behavior change, and the next PR does that one thing.
2. About a week of real use produces no code change. Record that in one short note and take row 3.

Use the daemon you actually talk to, on current `main`. In the live config:

- `chat-default` is a Chat hat (`component = "main-agent"`, no domain). It is not listed in
  `[chat] agent_profiles`.
- The specialist hats you actually open are listed in `[chat] agent_profiles`. `operator` is the
  creator hat.
- `chat-search` stays a commented opt-in until you miss history search. The example is
  `config.example/policy.toml`.
- Optional: `[providers.fallback]` on the paid provider. Copy the shape from
  [`tuning.md`](../spec/reference/tuning.md). The header is `[providers.fallback]`, a table inside
  the `[[providers]]` entry.

Write one note per issue. Keep the notes locally. Do not commit a scratch log. A note is ready for
a PR when it names the behavior you saw and the behavior you want.

## Acceptance context

### R1 — Codex-only daemon PR review dogfood

Use one repository and one writer. Start a PR as draft on a branch that is absent from the daemon
host, mark it ready, and let the configured checks become green. Prove one clean COMMENT and one
blocker COMMENT plus checklist and draft conversion. Restart between GitHub publication and ledger
completion, then prove that marker reconciliation creates no duplicate. Push a new commit and prove
that it does not run until a new `ready_for_review` transition arms that exact SHA.

Record the image SHA, PR/SHA review keys, GitHub review and checklist IDs, daemon restart evidence,
and the absence of a second writer. Do not begin Slice 4 worker fallback until this proof passes.

### 0.7 / C3 — controlled cross-harness baseline

The experiment spec is [`cross-harness-baseline.md`](cross-harness-baseline.md). That file is the
authority for what C3 is, what does not close it, the adapter gap, and the published-report bar.

Short form: instrument pinned Liberado, Pi, Hermes, and Deep Agents. Use the same task, repository
commit, model, provider, sampling settings, and resource limits. Keep each harness's native system
prompt and tool schemas. Run repeats where cost permits.

Report:

- Ship-gate and merge-ready rate.
- Cost per accepted result.
- p50 and p95 duration where the sample permits them.
- Human repair required.
- Trace-linked failure classes.

A single comparison without Hermes is evidence, not this baseline. Do not rank harnesses or change
`max_turns` from that sample. Runner contract:
[`harness-comparisons.md`](../spec/reference/harness-comparisons.md). Cost-lever research:
[`harness-study-2026-08.md`](harness-study-2026-08.md).

### C5 — completion-gate measurement

Run the same controlled task set as C3
([`cross-harness-baseline.md`](cross-harness-baseline.md)) with `[coder.gate] enabled` off and on.
Measure accepted results, model calls, cost, and repair. The gate costs `1 + fresh_reviewers` model
calls per attempt. Keep it default-off until the result supports a change.

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

### Coding-worker control plane

Liberado as scheduler over interchangeable coding CLIs. Draft:
[`coding-worker-control-plane.md`](coding-worker-control-plane.md). Do not implement from that
file. C3 is the evidence gate. The native loop is one worker, not the thing to make dramatically
smarter in the meantime.

## Branch and integration rule

Before creating a branch, record these fields in the local `current_unmerged_work.md` at the repo
root (listed in `.git/info/exclude`, never committed):

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
