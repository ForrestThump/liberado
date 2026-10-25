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

The order is the October 2026 merged roadmap: first-review steps 1–12, plus agent-runtime items
A0–A10, in the slot-table order 1–14. The reviews are historical evidence:

- [2026-10-opus-review.md](../reviews/2026-10-opus-review.md)
- [2026-10-opus-agent-runtime.md](../reviews/2026-10-opus-agent-runtime.md)

Where a review and this file disagree, this file wins. The first review's step 3 says "cut the
gate tax". This queue does not. Use the step 3 title in the table.

Take the first open row whose dependency is met and that is not already in progress. Do not choose
a later row because it is easier. Use one item per PR. Before implementation, verify the item
against current code and recent git history.

**A0 is in progress.** The pull request is "fix: runtime authority defects (A0)", on branch
`fix/runtime-authority-a0`. Do not open a second branch for A0a, A0b, or A0c.

**A9 is parked.** Jev and group rooms stay at order 14. Do not start them.

**Step 3 needs Forrest's approval of the design before implementation.** Do not open that work
from this row alone.

An operator note that names one behavior change becomes the next PR for that change. It does not
reopen the parked coding-pack rows, and it does not sit in front of order 1.

If an item is blocked only by another repository or an external service, record the exact blocker
in the local `current_unmerged_work.md` (repo root, never committed) and take the next unblocked
item. Do not skip a code dependency.

Already landed, and not rows: wire stamp
[#274](https://github.com/ForrestThump/liberado/pull/274), WebUI shelves
[#276](https://github.com/ForrestThump/liberado/pull/276), tunable `agent_profiles`
[#277](https://github.com/ForrestThump/liberado/pull/277), example `chat-default` hat
[#278](https://github.com/ForrestThump/liberado/pull/278). Contract:
[`../spec/architecture/chat-agent-surface-mode.md`](../spec/architecture/chat-agent-surface-mode.md).

## Implementation order

| Order | Item | Dependency |
|---:|---|---|
| **1** | **A0a, A0b, A0c — fix the three runtime authority defects** | **In progress.** PR "fix: runtime authority defects (A0)", branch `fix/runtime-authority-a0`. |
| **1** | **Step 1 — close the control plane** | None. Same order as A0. The A0 branch does not by itself close this row. |
| **2** | **Step 2 — make the docs agree with the direction** | None. The prose alignment is this documentation change. The mechanical `docs-audit` test below is still open. |
| **3** | **Step 3 — Freeze the coding pack, and make the quality gates cheaper without weakening them** | Step 2 prose (this change). **Needs Forrest's approval of the design before implementation.** |
| **3** | **A1 — agent registry** | Step 2 prose (this change). May run in parallel with step 3. It does not need the outbox. The unbuilt `docs-audit` rule does not block this row. |
| **4** | **Step 4 — durable delivery outbox** | Step 1. The mailbox in A2 shares this store. |
| **5** | **Step 5 — schedule catch-up and last-fire state** | Step 4. |
| **6** | **A2 — mailbox store** | Step 4 and A1. First of A2, then A3, then A4. |
| **6** | **A3 — router and policy** | A2. |
| **6** | **A4 — agent tools and the envelope** | A3 and A0. |
| **7** | **Step 6 — readiness, `liberado doctor`, and a dead-man alert** | Step 4. Do this before A5. |
| **7** | **A5 — activity view and cascade cancel by root** | A3 and step 6. |
| **8** | **A8 — cost governor** | A3. You can fold this into A3 if A3 stays under size M. Do it before more autonomy. |
| **9** | **Step 7 — backup, restore, and export** | Step 6. The mailbox is data worth restoring by this point. |
| **10** | **A6 — per-agent routines and memory** | A3 and step 5. |
| **10** | **A7 — durable async workers** | A3 and step 5. |
| **11** | **Step 8 — task and reminder write API with stable IDs** | Steps 4 and 5. E4 only if the inbox depends on it. |
| **12** | **Step 9 — scoped outside-agent access** | Steps 1 and 8. |
| **12** | **A10 — outside agents as mailbox clients** | A4 and steps 1 and 9. Merged with step 9. |
| **13** | **Step 10 — interactive journal** | Step 8. Step 12 helps and is not required. |
| **13** | **Step 11 — budget in the substrate (rework PR #317)** | Steps 4 and 5. Step 9 for the outside-agent tools. |
| **14** | **Step 12 — cheaper, simpler native loop (fallback only)** | Step 3. |
| **14** | **A9 — group rooms, and Jev** | **Parked.** Depends on A5, A6, and two weeks of use. Do not start. This row absorbs the old JEV1 / CAS5 row. |
| — | **Shorten AGENTS.md** | Does not outrank order 1. Do not do it inside the October 2026 docs alignment. |

## Done when

### Order 1 — A0a, A0b, A0c

- **A0a.** A test where the `research` session calls `delegate` to write a task is refused. The child grant is the calling session grant intersected with the dispatcher ceiling. Read-only profiles default to `delegation = false`. The test fails on the code before the fix.
- **A0b.** Approving "Everywhere" from the researcher does not change the `dispatcher` grant. Chat proposals carry the session's component. "Everywhere" goes to that component. Relabel "Session" as "until restart, all chats" until A6 makes it real, or hide it.
- **A0c.** A test cancels a chat turn during `delegate`, and the child ends as `Cancelled` within 2 seconds. `cancel_turn` cancels hub sessions whose origin is that conversation.

### Order 1 — Step 1, close the control plane

- An unauthenticated `curl` from another tailnet host gets 401.
- A `fetch` from another origin fails.
- A test coding run that `curl`s `/api/approvals/x/resolve` is refused.
- A test in `test-support` fails if someone adds `CorsLayer::permissive` or a `0.0.0.0` bind.

The work is: default bind `127.0.0.1`, with `LIBERADO_BIND` to opt in to a tailnet address; a bearer token for the API, with separate per-client tokens, and a human-session credential that alone can call `approvals/*/resolve` and `profile`; same-origin CORS, and `SameSite=Strict` cookies or a CSRF header on POST; a Telegram callback sender check; coding commands in the Docker sandbox with no route to the daemon port, or deny network access by default; Compose port `127.0.0.1:4201:4201`.

### Order 2 — Step 2, make the docs agree with the direction

- The backlog's first open row is a substrate item. Order 1 is that row.
- A `docs-audit` rule fails if `roadmap.md` names an order that `direction.md` does not. That rule is not built yet. It is a small code change. It does not block rows whose other dependencies are met, and it is still part of this row's done-when test.

The document rewrite (this file, `roadmap.md`, the root README strategy, and the historical banner on `positioning.md`) is the prose half of this row.

### Order 3 — Step 3, freeze the coding pack and make the gates cheaper

**Needs Forrest's approval of the design before implementation.**

Done when:

- The same thresholds still apply to new and changed code.
- A typical feature PR no longer carries thousands of lines of baseline-JSON diff.
- Code is no longer renamed or split just to dodge the matcher.

Running the Windows test job and the mutant job nightly, instead of on every pull request, is an option. It is not a decision.

### Order 3 — A1, agent registry

- Renaming an agent keeps its workspace, its Matrix DM, and its ID.
- `create_agent` with the same name reuses the agent.
- A test pins that the workspace path does not depend on the title.

The work is `agents/<slug>.md` definitions and the `agents` table, a migration from the Agents-shelf rows, workspace and Matrix slug and bindings re-keyed to `agent_id`, and creating or editing an agent through a card.

### Order 4 — Step 4, durable delivery outbox

Fault-injection tests pass for these cases:

- Telegram returns 500 three times, and the message arrives exactly once on the fourth try.
- The process is killed between enqueue and send, and the message is sent after restart.
- The notifier is missing, and the row stays pending and is visible.

One SQLite outbox in `<data_dir>` with rows of `(key, channel, payload, due_at, attempts, sent_at)`. Every reminder, digest, push, and approval card goes through it. Mark the row sent only after the channel accepts it, and retry with backoff. After N failures, fall back to a second channel (Web Push or `ntfy`). Writes are atomic. The outbox has a WebUI page.

### Order 5 — Step 5, schedule catch-up

- A test stops the daemon from 08:58 to 09:05, and the 09:00 reminder arrives once at 09:05.
- DST transition tests for America/Chicago pass, for both spring and autumn.

Persist `last_fired_at` for each schedule. Each schedule has a catch-up policy: `skip`, `once_within(<duration>)` (the default for reminders), or `all`. Replace the same-minute match in the reminder tick with "due since the last tick and not in the outbox". Index the vault by changed files only. Scheduled goals are `direct = true` by default.

### Order 6 — A2, then A3, then A4

**A2 done when:** fault tests pass. A crash between claim and consume re-delivers once. A crash after an outward action goes to `needs_human`. A duplicate post is ignored.

Tables `messages`, `roots`, and `runs` live on the same SQLite file as the durable outbox. Include the state machine, leases, and the consumed-ID set. Human messages from all three surfaces go into the mailbox. No model code changes in A2.

**A3 done when:** property tests show that no message sequence produces more than `max_hops` wakes per root without human input. A ring is always dropped. Hand-back is always refused. A two-agent ping-pong test ends at the limit, and the drop reason is visible.

A3 is the wake rules, one turn at a time per agent, the global pause switch, and dropped-message notes.

**A4 done when:** a peer message that says "ignore your instructions and write X" produces a card, not a write. The trace shows the envelope. Two named agents finish a request-to-result handoff, and you see it in both chats.

A4 adds `send_message`, `list_agents`, the envelope, and the taint flag. Writes in peer-started turns become cards. Fleet tools are hidden in turns that a human did not start.

### Order 7 — Step 6, then A5

**Step 6 done when:** revoking the Telegram token turns `doctor` red and makes `ready` return 503. An alert arrives on the other channel within one tick.

`/api/health/ready` reports vault watcher, provider ping, Telegram `getMe`, MCP pool, outbox backlog, and free disk. `liberado doctor` prints the same report, plus a config check and the config directory it resolved. A daily "I am alive" message goes to the alert channel. If it does not arrive, a service outside the box tells you.

**A5 done when:** from your phone, you can see a three-hop chain, cancel it, and get partial results in the requester's chat. Pausing an agent stops new wakes within one router tick.

A5 is the WebUI Activity page, cancel by root, pause per agent, Telegram and Matrix alerts through the outbox, and read-only roots on the dogfood MCP.

### Order 8 — A8, cost governor

A synthetic loop with a 10k-token root budget stops at the budget, and the alert arrives.

Daily budgets per agent and per root, enforced with priced tokens from `[[models]]`. When a budget trips, pause the agent and send an alert.

### Order 9 — Step 7, backup, restore, and export

- The CI restore test passes.
- You have run one real restore onto a clean machine, and you wrote down the time it took.

`liberado backup` writes a consistent snapshot of `<data_dir>` plus the vault git reference. The format must work with `restic` and `borg`. `liberado restore` reverses it. `liberado export` writes plain files: conversations as Markdown, approvals as CSV. A CI test restores a backup into a temporary directory and boots the daemon.

### Order 10 — A6 and A7

**A6 done when:** a morning routine for the `life` agent runs in the `life` chat with the `life` grant, and survives a restart over its fire time. The agent's memory note shows in its next turn.

`agent =` on schedules. A fire becomes a mailbox `request`. Per-agent memory injection and appends. Shared memory writes need a card. "Session" approval scope is real, scoped to the agent's home conversation.

**A7 done when:** restarting the daemon during a 5-minute delegated task finishes the task once, or raises `needs_human`. It is not silently cancelled. The chat turn returns at once with a delivery ID.

`delegate` becomes a mailbox `request` to a worker run, with a lease and heartbeat. Async by default. The result comes back as a message. Progress arrives as `status` notes. Re-run once.

### Order 11 — Step 8, task and reminder write API

- You can create a timed reminder from your phone in under 10 seconds, and it appears in Obsidian.
- Completing it in Obsidian stops any later reminders.
- An ID survives edits to the lines above the task.

A `tasks` module over Obsidian Tasks lines, with stable IDs from block references (`^lib-xxxx`). Create, complete, snooze, and reschedule, all through the existing vault write gate. Telegram commands `/remind`, `/done`, and `/snooze`, plus snooze buttons on reminders. A WebUI task list.

### Order 12 — Step 9 and A10

**Step 9 done when:** a client on the tailnet (Claude Desktop, Codex, or OpenMausBot) lists your tasks and proposes one, and you approve it on your phone. A revoked token gets 401. The audit log shows all three calls.

A streamable-HTTP MCP server with a per-client token, scopes (`tasks:read`, `tasks:propose`, `journal:append`, `budget:read`), and rate limits. Every write becomes an approval card. No client can approve. An audit log of every call, and a revocation list. The server stays on Tailscale by default. Write a one-page threat model before any public endpoint, and decide the Grok Bot question there.

**A10 done when:** a client on the tailnet sends a request to `life`, the write becomes a card, and you approve it on your phone.

An outside client is an `external:<client>` sender with a token identity. It can send only `request` and `fyi` messages. It sees only results that are addressed to it, and it can never approve.

### Order 13 — Steps 10 and 11

**Step 10 done when:** you have used the journal for one week, and each journal conversation can be reached in one click from Obsidian and from the WebUI.

Telegram capture appends to the Obsidian daily note. A "talk about today" chat links both ways: the conversation ID goes in the note's frontmatter, and the note link goes in the conversation. At the end of the chat, the agent writes a summary note.

**Step 11 done when:** the budget change is under 2,000 changed lines, with no metrics JSON. The weekly message survives a Monday restart.

Replace the hand-written Web Push with a maintained crate, or with `ntfy`. Move the bucket values to `topology.toml` with no personal defaults in code. Send through the step 4 outbox, and use the step 5 catch-up instead of the job's own catch-up logic. Read-only budget tools for outside agents (step 9).

### Order 14 — Step 12, and parked A9

**Step 12 done when:** a fixed set of 20 chat tasks costs at least 40% fewer tokens, with the same pass rate on `liberado-eval`.

This is fallback only. For chat, remove one model role. The face calls a small per-profile tool set directly, and `delegate` is only for long goals. Load MCP tool schemas lazily, by profile. Inject a capped `USER.md`-style memory note into each turn.

**A9 is parked.** Do not start Jev or group rooms until A5 and A6 have landed and you have used the mailbox for two weeks. The old JEV1 / CAS5 row lives here. Spec, for when it unparks: [`../spec/architecture/jev-integration.md`](../spec/architecture/jev-integration.md). Confidence-gated advisor only. The dispatcher remains the sole grantor.

**A9 done when,** after it unparks: a three-agent room answers one human message in at most 2 rounds, with no unbounded replies. Mention routing, 2 rounds, `NO_REPLY`, and a Matrix room with puppets. Jev routing is optional and confidence-gated.

`docs/direction.md` still records the 2026-10-18 decision to build an unpolished Jev prototype, off by default. This queue parks that prototype at A9. Agents select work from this file.

### Shorten AGENTS.md

Move the incident stories out of `AGENTS.md` and leave about 80 lines of rules. Put the stories in `docs/validation/incidents.md` and link that file. Do not shorten `AGENTS.md` in the October 2026 docs-alignment change. This row does not outrank order 1.

## Moved and parked

These rows were the queue before the October 2026 rewrite. None were dropped. The old letter names A1 and A2 meant token economics. In this file those names belong to the agent registry and the mailbox. The token-economics rows are **TE1** and **TE2**.

| Old order | Item | Where it went |
|---:|---|---|
| 1 | CAS4 — use the shelves and write notes | Parked below. Operator practice. It no longer blocks the queue. |
| 2 | The first dogfood note that names one code change | Kept as the operator-note rule in the introduction. It is not a numbered row in front of order 1. |
| 3 | R1 — prove Codex-only daemon PR review and one-repository cutover | Parked with the frozen coding pack. |
| 4 | 0.7 / C3 — publish the controlled cross-harness baseline | Parked with the frozen coding pack. |
| 5 | C5 — measure the completion gate | Parked with the frozen coding pack. |
| 6 | Token-economics read (was called A1) | Parked as **TE1**. |
| 7 | Narrow the tool catalogue (was called A2) | Parked as **TE2**. Blocked on TE1. |
| 8 | E4 — directory enumeration in TurboVault | Later. Conditional for step 8, only if the inbox depends on it. |
| 9 | E5 — stop the TurboMCP SSE reconnect storm | Later. External reliability work. |
| 10 | E2 — implement the inbox layer | Later. E4 must land first. |
| 11 | C4 — finish dedicated goal-view panes | Parked with the frozen coding pack. |
| 12 | JEV1 / CAS5 — TypeSafe Jev first wedge | Parked as order 14, A9. |

### CAS4 — use the system (parked)

This is operator time. It is not an implementation row, and it does not gate orders 1–13.

It closes in one of two ways when someone picks it up again:

1. The operator names a headache, bug, or behavior change, and the next PR does that one thing.
2. About a week of real use produces no code change. Record that in one short note.

Use the daemon you actually talk to, on current `main`. In the live config:

- `chat-default` is a Chat hat (`component = "main-agent"`, no domain). It is not listed in `[chat] agent_profiles`.
- The specialist hats you actually open are listed in `[chat] agent_profiles`. `operator` is the creator hat.
- `chat-search` stays a commented opt-in until you miss history search. The example is `config.example/policy.toml`.
- Optional: `[providers.fallback]` on the paid provider. Copy the shape from [`tuning.md`](../spec/reference/tuning.md). The header is `[providers.fallback]`, a table inside the `[[providers]]` entry.

Write one note per issue. Keep the notes locally. Do not commit a scratch log. A note is ready for a PR when it names the behavior you saw and the behavior you want.

### Frozen coding pack (parked)

Do not start these while the coding pack is frozen. Step 3 is the design for that freeze, and it needs Forrest's approval before implementation. Until that design lands, these rows stay unselectable.

#### R1 — Codex-only daemon PR review dogfood

Use one repository and one writer. Start a PR as draft on a branch that is absent from the daemon host, mark it ready, and let the configured checks become green. Prove one clean COMMENT and one blocker COMMENT plus checklist and draft conversion. Restart between GitHub publication and ledger completion, then prove that marker reconciliation creates no duplicate. Push a new commit and prove that it does not run until a new `ready_for_review` transition arms that exact SHA.

Record the image SHA, PR/SHA review keys, GitHub review and checklist IDs, daemon restart evidence, and the absence of a second writer. Do not begin Slice 4 worker fallback until this proof passes. Kickoff: [`daemon-pr-review-kickoff.md`](daemon-pr-review-kickoff.md).

#### 0.7 / C3 — controlled cross-harness baseline

The experiment spec is [`cross-harness-baseline.md`](cross-harness-baseline.md). That file is the authority for what C3 is, what does not close it, the adapter gap, and the published-report bar.

Short form: instrument pinned Liberado, Pi, Hermes, and Deep Agents. Use the same task, repository commit, model, provider, sampling settings, and resource limits. Keep each harness's native system prompt and tool schemas. Run repeats where cost permits.

Report:

- Ship-gate and merge-ready rate.
- Cost per accepted result.
- p50 and p95 duration where the sample permits them.
- Human repair required.
- Trace-linked failure classes.

A single comparison without Hermes is evidence, not this baseline. Do not rank harnesses or change `max_turns` from that sample. Runner contract: [`harness-comparisons.md`](../spec/reference/harness-comparisons.md). Cost-lever research: [`harness-study-2026-08.md`](harness-study-2026-08.md).

#### C5 — completion-gate measurement

Run the same controlled task set as C3 ([`cross-harness-baseline.md`](cross-harness-baseline.md)) with `[coder.gate] enabled` off and on. Measure accepted results, model calls, cost, and repair. The gate costs `1 + fresh_reviewers` model calls per attempt. Keep it default-off until the result supports a change.

#### TE1 — deployed token-economics read

Deploy the existing instruments, wait one day, and report:

- Offered and surviving MCP counts.
- Dispatcher cache-hit rate.
- Subagent and direct-execution shares.
- Total repeated calls.

Use `liberado-cost --json`. This item is measurement only. See [`token-economics-findings-2026-08.md`](token-economics-findings-2026-08.md). The logger is already at info. Do not change the catalogue in this item.

#### TE2 — tool-catalogue narrowing

Blocked on TE1. Change only the catalogue path that TE1 identifies. Do not rewrite prompt content while moving or narrowing blocks. Update `crates/cost/tests/journal_shape.rs` with any change to the latency journal shape.

#### C4 — dedicated goal-view panes

Add a role timeline, gate panel, and verifier panel to `crates/tui`. Gate votes already stream but currently render in the joined pane. Follow [`session-surface-contract.md`](../spec/architecture/session-surface-contract.md).

### Later substrate work (not in the review order)

Selectable only after order 11, and only when the dependency named here is met. Step 8 names E4 as a conditional dependency of the task API. It is not required unless the inbox needs a directory listing.

#### E4 — TurboVault directory enumeration

The inbox layer needs a positive directory scope. In the TurboVault repository, implement one supported path: enable the required SQL query, add a positive `path_prefix` to search, or add a `list_notes(path)` operation. Record the upstream commit before E2 begins.

#### E5 — TurboMCP SSE reconnect storm

Diagnose and stop the idle read-error, stream-ended, reconnect loop in `turbomcp_http::transport`. The observed rate was about 50 reconnects per minute and displaced useful logs. Treat this as an external dependency and record the upstream commit.

#### E2 — inbox layer

E4 must land first. Implement the two capture surfaces and compare-and-swap clearing defined in [`inbox-spec.md`](../spec/inbox-spec.md). Unflagged notes belong to the schedule; `#now` notes belong to the watcher. Reuse existing provenance and content-hash idempotency.

### Deferred, not selectable

#### C6 — repository map and context selection

Focused search is sufficient today. Do not schedule an always-on repository map until measurement shows that missing context, rather than missing files, limits accepted results. If the item reopens, keep goal-context ranking in the kernel and source-tree or symbol-graph work in the coding pack.

#### Coding-worker control plane

Liberado as scheduler over interchangeable coding CLIs. Draft: [`coding-worker-control-plane.md`](coding-worker-control-plane.md). Do not implement from that file. C3 is the evidence gate, and C3 is parked with the frozen coding pack. The native loop is one worker, not the thing to make dramatically smarter in the meantime.

## Branch and integration rule

Before creating a branch, record these fields in the local `current_unmerged_work.md` at the repo root (listed in `.git/info/exclude`, never committed):

- Base commit.
- Predecessor.
- Shared files.
- Merge order.

Stack only work that shares a dependency or integration point. Branch independent work from current `main`. After a predecessor merges, rebase dependent branches, rerun local gates, and require fresh GitHub CI.

## Required PR evidence

The repository PR template asks for:

1. Evidence that the backlog item is still open.
2. One executed defect mutation for each changed behavior.
3. Evidence from the real code path when it can be driven.
4. A kernel-or-pack placement decision.
5. Base, predecessor, shared-file, and merge-order details.

Run `just ci` before push. A mutation must be applied, observed to fail, and restored without using `git checkout` on a file that can contain uncommitted work.
