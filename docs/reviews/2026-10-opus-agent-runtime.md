---
kind: finding
status: historical
authority: evidence
domain: product
canonical_for: opus-agent-runtime-2026-10
---

# Liberado agent runtime: a focused second review (October 2026)

> Point-in-time external review of `main` at `f027c155` (October 2026). It is not current truth. Where a claim here disagrees with the backlog, [the backlog](../future-work/backlog.md) wins. The slot table below is the backlog order. Step 3's backlog title is the approved wording, not "cut the gate tax".

**Reviewer:** Claude (Anthropic).
**Scope:** `main` at `f027c155`. This review covers multi-agent work, workers, approvals across
agents, and how they fit `docs/direction.md`. It adds to `2026-10-opus-review.md` and changes
one point in it. I read the code; I built nothing.
**Status:** a review artifact. It is not committed.

**Change to the first review.** The first review listed multi-agent teammates as a non-goal. That
was a misreading. The direction defers an *optimal* coordination substrate. It does not rule out
specialist agents that message each other, and on 2026-10-12 you approved more Grok Bot-like
agents. This review treats multi-agent as in scope. It also argues that the right shape is a
substrate feature (a durable mailbox), not a chat feature. Built that way, it passes the
direction's decision filter: the mailbox stays useful even when the agents on it are rented.

---

## 0. Summary

- **What you have:** specialist *chats*, not specialist *agents*. An "agent" is a conversation
  with an agent profile. It has a private file folder and an optional Matrix DM. It can call
  `delegate` to run an anonymous worker, synchronously. Agents cannot message each other. The
  spec says so on purpose: "no peer mesh".
- **Of your four approved items, two are partly there and two are missing.** Direct agent chat
  is partly there (the Agents shelf and Matrix DMs). In-chat approval is partly there (cards,
  but they are not tied to the agent that asked). Agent handoffs are missing. Long-running work
  is missing: delegated work dies on restart.
- **Three existing defects must be fixed before you add anything.**
  - `delegate` gives a narrow agent the dispatcher's wider authority.
  - Approvals go to a shared pool, not to the agent that asked.
  - Cancelling a chat turn does not stop its delegated child.
- **The references have converged on one design:**
  - async messages to long-lived agents;
  - a harness-written envelope that marks peer text as untrusted data;
  - a hop limit that resets on human input;
  - cancel that cascades through the whole tree;
  - durable runs with leases;
  - no agent may ever approve for another.
- **Proposed design:** a substrate-owned **mailbox** on the same SQLite store as the durable
  outbox from the first review. A deterministic **router** is the only code that wakes an agent.
  Agents send typed messages and never call each other directly. Writes in turns that a message
  starts become approval cards, tied to the agent and to the human request that started the
  chain.
- **Sequencing:** fix the three defects as part of the security step (step 1). The agent
  registry can come early. The mailbox must wait for the durable outbox and schedule catch-up
  (steps 4 and 5). Otherwise you build a second delivery path with the same at-most-once flaws.

---

## 1. What Liberado has today

### 1.1 Agent definitions and identity

- **An agent is a conversation, not a record.** The spec "locks Reading B": an agent is "a
  long-lived specialist chat" (`docs/spec/architecture/chat-agent-surface-mode.md` §1). The
  stamp is `surface_mode = Agent` when the session's profile is in `[chat] agent_profiles`
  (`crates/main-agent/src/sessions/surface_mode.rs`).
- **The spec forbids** a new `SessionGrant` field, a new `SessionKind`, and any "peer agent
  mesh / A2A" (§2, §5).
- **Profiles** are `[[session_profiles]]` rows (`config.example/topology.toml:591-629`). A
  profile names a policy `component`, and the component's grant is in `policy.toml`. Example
  agents: `coding`, `life`, `researcher`, `operator`.
- **Identity has three keys that can drift apart:**
  - `create_agent` dedupes on the case-sensitive pair (profile, title). "Renaming changes
    identity" (§6e).
  - The workspace and the Matrix binding use the session ID
    (`crates/main-agent/src/sessions/workspace.rs:1-5`).
  - The Matrix puppet uses a slug that "stays the same after a rename" (PR #299).

  A fork is a new session ID, and therefore a new workspace.
- **Creating an agent** goes through the Agent Creator chat (`operator` profile) and the
  `create_agent` tool (`crates/main-agent/src/face/create_agent.rs`). It can only choose an
  existing profile. "Creators cannot pass custom capability lists". The persona is the shared
  face prompt, or the creator prompt for that one chat.
- **The `life` domain pack is a demo.** `LifeOpsDemoRunner` "completes" criteria and proves
  that packs can plug in (`crates/session/src/life_demo.rs:1-5`). There is no real life-ops
  worker. The `life` chat profile is a separate thing that uses the face loop.

### 1.2 Channels per agent

- **WebUI:** Chats and Agents shelves (#276). Approval cards show in the chat that raised them,
  and the WebUI polls for them every 3 seconds (#295).
- **Matrix:** one appservice puppet and one direct room for each agent. DM commands are `help`,
  `status`, `model`, `stop`, and `cancel`, and they act on that agent's session. The main room
  is `#liberado` (#299, `crates/matrix/`). Echoes from puppets are ignored, so agents cannot see
  each other in Matrix.
- **Telegram:** one sticky chat for the face, a reminder bot, and permission keyboards. It has
  no per-agent routing.

### 1.3 Delegation, dispatcher, orchestrator, executor

- **`delegate`** (`crates/main-agent/src/face.rs:80-187`):
  - It starts a hub session (domain `dispatch`) with `start_background`, and then waits for it
    with `await_terminal`. The chat turn is blocked until the child finishes.
  - It removes `AskHuman`, so the child cannot ask you a question.
  - The child's grant is `dispatcher_capabilities` with `profile: None` (`face.rs:121-136`).
    **It is not intersected with the calling agent's grant.** PR #296 audited this site and
    left it "deliberately process-wide".
  - In the example policy, `research` has one read-only vault grant. The `dispatcher` grant has
    task and decision writes, `tasks-mcp`, `memory-mcp`, and `code-dispatch`
    (`config.example/policy.toml:77-92,173-176`).
  - The `researcher` profile does not set `delegation = false`
    (`topology.toml:610-614`), so a read-only agent can delegate a write.
- **Dispatcher** (`crates/dispatcher/`): one classify call, which returns `ExecuteDirect`,
  `DispatchSubagent`, or `Clarify`. **Orchestrator** (`crates/orchestrator/`): runs the
  decision. **Executor** (`crates/executor/`): the bounded loop. Its limits are turns, wall
  clock, and tokens (`budget.rs:36-65`). The dispatcher grant is checked in code at invoke
  (`risk_gated.rs`). That part is good.
- **Parallel workers:** the orchestrator's parallel dispatch is limited by
  `max_concurrent_subagents` (default 2; `crates/orchestrator/src/lib.rs:1339`,
  `config-loader/src/model/tuning.rs:99`). The hub's `start_background` has no limit of its own
  (`crates/session/src/hub.rs:175`).
- **Async seed:** `POST /api/goals` with an `origin` conversation starts a background session.
  `spawn_return_handoff` adds a summary note to the parent conversation when the session
  finishes (`crates/server/src/api/goals.rs:255-336`). This is the only async handoff. The
  waiter is a `tokio::spawn` that a restart loses, and the note does not start a new turn in the
  parent.

### 1.4 Coder fan-out

The coding pack has its own isolated fan-out into worktrees, with its own concurrency setting
(`crates/coder-agent/src/session_pack/build.rs:356-426`, `crates/coder-agent/src/fanout/`). It
does not share a mechanism with the personal agents.

### 1.5 Schedules and automations

- A cron `Schedule` can name a `profile`, a `pool`, `direct`, `max_turns`, and `deliver`
  (`crates/cron/src/lib.rs:33-80`).
- A fire starts a *new* goal session under that profile's grant (`crates/daemon/src/react.rs:115-170`).
  The result goes to the notifier (`maybe_deliver_cron_result`), not into the agent's own
  conversation, memory, or Matrix DM. An agent does not own its routines.
- Edits from the Automation Editor take effect "after the service restarts" (§6f).
- Cron does not catch up after a restart (first review, R2).

### 1.6 Permissions, grants, and approvals for each agent

- **The session grant gates tool calls** (#296, `sessions/approval.rs:107-131`). This is
  correct at invoke time.
- **Approval attribution is not per agent:**
  - Chat proposals are tagged `DEFAULT_POOL` (`sessions/approval.rs:104-125`).
  - When you approve, the grant goes to `grant_component_for_pool(None) = "dispatcher"`
    (`crates/daemon/src/helpers.rs:48-53`, `crates/daemon/src/proposals.rs:410-423`).
  - **"Session"** is not one session. It is a process-wide grant for the whole pool, kept in
    memory until restart (`crates/common/src/session_grants.rs:1-6`).
  - **"Everywhere"** writes to the `dispatcher` component. Every agent can reach that component
    through `delegate`.

  Approving the researcher's request therefore widens a different, shared principal.
- **Peer approval:** the approve action is not a tool, which is good. But the HTTP route has no
  caller identity, and the coding shell can reach it (first review, R1).

### 1.7 Memory for each agent

- **Files:** each session has a private workspace (`crates/agent-workspace/`, 1 GiB cap, SSRF
  guard, hashed directory names). "There is no path that every agent can write."
- **Long-term memory:** `memory-store` and `memory-mcp` hold one general store and one
  procedural store for the whole daemon. They are not per agent, and they are recalled only by
  tool call (`crates/memory-store/src/lib.rs:1-8`). The prompt does not inject them.
- **Long conversations:** compaction runs at the turn boundary
  (`crates/main-agent/src/compaction.rs`). That keeps a long chat alive, but it is not memory.

### 1.8 Long-running work, cancel, and restart

- **Cancel:** `cancel_turn` aborts the chat turn's task and "persists nothing"
  (`crates/main-agent/src/sessions.rs:1211-1226`, `crates/server/src/api/chat.rs:358-384`). The
  delegated child was started with `start_background`, so it keeps running with the dispatcher's
  grant and no parent waiting for it. There is no cascade.
- **Restart:** drain parks running goals. On startup, a parked session that cannot `AskHuman`
  is cancelled as "orphaned after restart" (`crates/session/src/hub.rs:357-412`). Delegated
  sessions remove `AskHuman`, so **all delegated work is cancelled on restart.**
- **Visibility:** `GET /api/sessions` returns the parent-child edge. The WebUI does not draw it,
  and the roadmap lists "Mobile session view" as open. Delegated work is visible only through
  `.liberado/dispatches/*.jsonl` and logs.
- **Cost:** `liberado-cost` computes cost only when someone reads it (`crates/cost/src/lib.rs`).
  No per-agent or per-day budget stops anything.

---

## 2. How the references do persistent specialist agents

| Dimension | Grok Bot | OpenClaw | Hermes Agent | Rakazo | OpenMausBot |
|---|---|---|---|---|---|
| Identity | App-created Bot: name, job, its own chat; model is chosen by Cursor | `agents.entries.<id>` config: own workspace (`AGENTS.md`, `SOUL.md`), session store, model, tool allow and deny | A profile is a whole Hermes home: config, `SOUL.md`, memory, skills, cron, `state.db` | Postgres row: name, instructions, model, `parentBotId`; `spawn_bot` | `bots.json` row: instructions, engine, model, level; own workspace directory |
| Memory and skills | Per-Bot memory plus a few shared facts; skills shared | Per-agent memory search; per-agent skill allowlist | Fully isolated per profile | Per-bot memory plus `save_shared_memory`; skills per space | `MEMORY.md` per bot plus team memory |
| Agent-to-agent messages | Async message to a long-lived Bot; tool not documented | `sessions_send`: wait for a reply or fire-and-forget; re-admitted after restart; `agentToAgent` allowlist of pairs | `message_agent`: fire-and-forget, `delivery_id`, durable ingress queue; kanban comments | `message_bot` with intent request/result/question/status/fyi; reply comes back automatically | `coordinate_bots` (work, consult, notify; reply resume, poll, none); SQLite inbox |
| Group rooms | 2–6 Bots; Bots decide, or `@mention` | Broadcast groups (experimental): `maxRounds` ≤4, `NO_REPLY` | 2–6 Bots; 3 rounds, 10 messages per send | 2–10 bots; mentions; `handoff_to_bot` | Mentions, lead bot, or "Auto" (a model named **Jev** at ≥0.6 confidence, else the lead); ring detection |
| Subagents | Launches are auto-reviewed; limits not documented | `sessions_spawn`: depth 5, 5 children, 8 concurrent; child tools only narrow | `delegate_task` in background; depth 1 by default; 10 concurrent; children lose memory, messaging, and cron tools | `run_subagent`: one turn, no nesting, 4 parallel | Depth 4; per-root budgets of 24 requests, 48 executions, 48 wakes |
| Long tasks | Run in the cloud; run history | Subagents resume after restart if authority still holds; Subagents panel | Children are lost on restart; kanban has heartbeats and reclaim | Durable runs with leases, heartbeats, recovery scan | Re-run a cut turn once, unless it already did an outward action |
| Routines | One Bot per routine; many trigger sources | Automations with an owner | Cron per profile | `schedule_create`; child routines need approval | Per-bot routines; webhooks into rooms |
| Cancel | "Stop now" for each Bot; no cascade documented | `/stop` cascades through the tree | `/stop` recurses and returns partial output | Archive cancels runs; subagent shares the abort signal | Cascades through the tree |
| Approvals across agents | Only the human whose message started it can approve; cards raised by another Bot expire | Peer messages can require approval; a reply is not fed back to the target | Unattended and relayed turns default to deny | Fleet tools only in owner-started runs; config changes always need approval; `BOT_MESSAGE_MAX_HOPS = 6`, reset by human input; no hand-back to sender | Receiver's level, but a coordinator's level flows down; "no bot message approves a permission"; **default is Full access** |

Sources are the docs and repos listed in the first review. Repo paths: OpenClaw `docs/concepts/multi-agent.md`,
`docs/concepts/session-tool.md`, `docs/tools/subagents/*`, `docs/channels/broadcast-groups.md`;
Hermes `website/docs/user-guide/profiles.md`, `bot-mode.md`, `messaging/a2a.md`; Rakazo
`packages/core/src/bot-messages.ts`, `action-approval.ts`, `group-handoff.ts`, `docs/run-execution.md`;
OpenMausBot `docs/runtime-contract.md`, `docs/approval-levels.md`, `server/room-post-budget.ts`,
`server/host/delegated-level.ts`. Rakazo stands in for "Rokazo", which I could not find.

**Patterns worth copying:**
- Peer text is escaped and wrapped in a harness-written envelope that marks it as data
  (OpenClaw, Rakazo, OpenMausBot).
- A hop limit that resets on human input (Rakazo), and per-root budgets (OpenMausBot).
- No hand-back to the sender, and a reply is not fed back to the target (Rakazo, OpenClaw).
- Children lose memory, messaging, cron, and fleet tools (Hermes, Rakazo).
- Cancel cascades through the tree and returns partial results (OpenClaw, Hermes, OpenMausBot).
- A cut turn re-runs at most once, and never after an outward action (OpenMausBot).
- Durable runs with leases and heartbeats (Rakazo, Hermes kanban).

**Patterns to avoid:**
- A shared computer with no isolation between agents (Grok Bot, Rakazo Team Computer).
- No hop limit and no master switch for agent-to-agent traffic (Grok Bot).
- Full access by default, and a coordinator's level that flows down (OpenMausBot).
- Turn budgets kept only in memory (OpenClaw broadcast groups).
- Recursion with no depth ceiling (Hermes above depth 1).

**A note on Jev.** OpenMausBot's room auto-router is also named Jev. It does the same job as your
planned Jev: it picks a bot at ≥0.6 confidence and otherwise falls back to the room lead. That is
working precedent for your JEV1 design: a confidence-gated advisor with a deterministic fallback.

---

## 3. Gap analysis

| Need | Liberado today | Gap | Severity |
|---|---|---|---|
| Stable agent identity | Three keys: (profile, title), session ID, Matrix slug | No agent record; a rename or fork splits the identity | High (it gets costlier to fix later) |
| Per-agent persona, model, and memory | Profile → grant and optional model; shared face prompt; global memory | No persona file and no per-agent memory, so agents differ only in tools | Medium |
| Agent-to-agent messages | None ("no peer mesh") | No send, inbox, or reply | High (an approved feature) |
| Handoffs | `delegate` to an anonymous worker, synchronous | No handoff to a *named* agent; the chat turn blocks | High |
| Authority across a hop | Child gets the dispatcher grant, not the parent ∩ worker | **Authority widens** | **Critical (existing defect)** |
| Approval attribution | Pool `default` → component `dispatcher`; "Session" means the whole pool | The approval widens a shared principal | **High (existing defect)** |
| Cancel | Aborts the face turn only | **No cascade**; orphaned workers keep running | **High (existing defect)** |
| Long-running work | Delegated sessions are cancelled on restart; handoff waiter is in memory | Not restart-safe | High |
| Routines per agent | Schedule → new session under a profile → notifier | The agent does not own its routines; results skip its chat | Medium |
| Human visibility | Dispatch journal files; no tree in the WebUI | You cannot see or steer the traffic | High |
| Loop and cost guards | Turn, wall-clock, and token limits per execution | No hop limit, no root budget, no daily cap per agent, no pause switch | High, once messaging exists |
| Injection between agents | Tool results reach the model as plain text | No envelope and no taint | High, once messaging exists |
| Group rooms | None | None | Low (park it) |

---

## 4. Target design

### 4.1 Principles

These come from `direction.md`, and from the event-bus advice in your own
`docs/future-work/research/agent_pools_research_results.md:310-650`.

1. **The substrate owns the mailbox.** Messages, runs, and receipts are substrate data in
   `<data_dir>/liberado.db`, the same SQLite file as the outbox from the first review (step 4).
   An agent is a client of the mailbox, and a rented agent (Grok Bot or Claude) is a client of
   the same mailbox.
2. **Only the router wakes agents.** The router is deterministic code. A message never starts a
   turn directly. The router decides, by rules that you can read, whether a delivery starts
   work, and with what authority.
3. **Authority never grows across a hop.** A turn's grant is computed by code. The model cannot
   name a grant. A peer message cannot approve, create agents, edit schedules, or widen a grant.
4. **Peer text is data.** It is escaped, wrapped in an envelope, and tainted. Taint gates
   outward actions in code.
5. **Every message is async.** No agent ever blocks on another agent. This removes deadlock
   (A waits on B, B waits on A) by construction.
6. **Tailscale-only.** The design adds no new listener. Every surface stays a client of the
   authenticated API from step 1.

### 4.2 Agent registry (identity)

**Definition file:** `agents/<slug>.md` in the vault. Frontmatter is the machine part; the body
is the persona. You can read it, edit it, back it up with git, and review it as a diff. Fields:

- `id`: a ULID that never changes.
- `slug` and `name`.
- `profile`: the grant component.
- `model` and `provider`: optional.
- `memory`: the path of the agent's memory note.
- `peers`: the agents it may send `request` messages to.
- `accepts_from`: the agents it accepts messages from.
- `budget`: `daily_tokens`, `max_hops`, `max_concurrent`.
- `routines`: names of the schedules it owns.
- `status`: `active`, `paused`, or `archived`.

**Runtime row:** an `agents` table in SQLite holds the state: home conversation ID, workspace
path, Matrix puppet, last active time, today's spend, and the paused flag.

**Keys:** the workspace, Matrix puppet, channel bindings, approvals, memory, and routines are all
keyed by `agent_id`, never by session ID or title. A rename changes `name` only. A fork makes a
new conversation for the *same* agent, unless you ask for a copy.

**Changes need a card:** creating an agent, or editing `profile`, `peers`, or `budget`, goes
through an approval card. This follows Rakazo's rule: "changes to instructions, models or groups
always need explicit approval".

### 4.3 Mailbox (the message model)

**Table `messages`** has these fields:

- `id`: a ULID; it is also the idempotency key.
- `root_id`: the human message, schedule fire, or webhook that started the chain.
- `parent_id`.
- `from` and `to`: an agent ID, `human`, `schedule:<name>`, or `external:<client>`. **The kernel
  sets `from`.** The model never sets it.
- `kind`: `request`, `result`, `question`, `status`, or `fyi`.
- `body`: up to 8 KB of text, plus optional vault links (not inline file contents).
- `hop`.
- `state`: `queued` → `claimed(lease_until)` → `consumed` → `answered`, or `expired`, `dropped`,
  `dead`, `needs_human`.
- `attempts`, `created_at`, and `cost_tokens`.

**Delivery guarantee:** at-least-once delivery into a turn, exactly-once effect on the record.

- The router claims a message with a lease.
- The turn record stores the IDs of the messages it consumed. A message that is already
  consumed is never fed again.
- If the lease expires (a crash), the message is re-queued **once**, unless the interrupted turn
  had already done an outward action (a vault write, a channel send, an MCP write). Then the
  message goes to `needs_human`, and you get a card. This is OpenMausBot's rule, made
  deterministic.

**Human messages use the same path.** Human messages from the WebUI, Telegram, and Matrix DMs
enter the agent's mailbox with `from = human`. This gives every surface one path. It also gives
the hop counter a clean reset point.

### 4.4 Router (wake rules)

Each agent is an actor. It runs **one turn at a time** and works through its mailbox in order.
The router starts a turn for message M to agent B only if all of these are true:

1. B is `active`, and the global switch `agents.paused` is off.
2. `M.from` is `human`, or B lists `M.from` in `accepts_from`, and the sender lists B in `peers`
   (for `request` messages).
3. `M.hop` ≤ `B.max_hops`. The default is 4. A `human` message resets the count to 0.
4. The root budget is not used up. The default root budget is 200k tokens across all agents and
   workers in that chain.
5. B's daily token budget is not used up.
6. **No ring:** B does not already appear as the *target of a request* earlier in the same
   root's chain, with an open request.
7. **No hand-back:** a `request` from B cannot go to the agent that just sent B a request in
   the same chain. B answers with a `result` instead.
8. Rate limits: at most 10 messages per minute per sender-recipient pair, and 30 per minute per
   root.

A message that fails a rule goes to `dropped`, with a reason that you can read. The router
tells the sender's home conversation. It does not wake the sender again.

**`result` messages:** a `result` wakes the requester exactly once and counts as a hop. A
`result` can never be answered with a `result`. `status` and `fyi` messages never wake anyone;
they are added to the recipient's conversation as notes. That is how "progress" works without
burning turns.

### 4.5 Authority for a turn

- **A human message:** the turn runs with B's own grant. This is today's behavior.
- **A peer `request`:**
  - Reads use B's own grant.
  - Every write or outward action becomes an approval card. The card names the root, the chain
    (`human → A → B`), the action, and B.
  - B's profile may list a few `autonomous_on_request` actions that skip the card (for example,
    appending to its own memory note).
- **Fleet tools** (`create_agent`, schedule editors, profile changes, `send_message` to more
  than one peer at once) are available only in turns that a human started. This is Rakazo's
  rule.
- **Workers** (the new `delegate`): worker grant = **calling turn's grant ∩ worker ceiling**,
  minus `AskHuman`, `send_message`, memory writes, and schedule tools (the Hermes rule). This
  fixes the defect in §1.3.
- **Approval attribution:**
  - Each card carries `agent_id` and `root_id`.
  - "Once" covers this call only.
  - "Session" covers this agent's current home conversation, and nothing else. Rename the
    current pool-wide behavior, or remove it.
  - "Everywhere" appends the grant to *this agent's* profile component. It never goes to
    `dispatcher`.
  - Only the human credential from step 1 can resolve a card.

### 4.6 Injection between agents

- **Envelope:** peer bodies are escaped and wrapped by the harness, as
  `<peer_message from="research" kind="request" hop="2" trust="untrusted">…</peer_message>`.
  The system prompt says that the envelope's content is data, not instructions.
- **Taint:** a turn whose input includes untrusted content (a peer message, a webhook payload,
  or an outside fetch) is marked tainted. In a tainted turn, outward actions always need a card,
  even when the grant allows them. This is a code check, not a prompt rule.
- **Size and form:** an 8 KB body cap. Remove markup that looks like a tool call. Send links,
  not raw file dumps.

### 4.7 Long-running work and progress

- **Runs:** worker runs become rows in `runs`, with a lease and a heartbeat every 30 seconds.
  The current park-and-cancel-on-start rule is replaced by the re-run-once rule in §4.3.
- **Progress:** progress events become `status` messages to the requester's home conversation.
  They are notes, not turns.
- **`delegate`:** it becomes async by default. It returns a `delivery_id`, and the result
  arrives later as a `result` message. A short synchronous form stays for quick lookups, with a
  60-second ceiling.

### 4.8 Cancel and pause

- **Cancel by root:** set `roots.cancelled = true`. The router stops delivering for that root,
  every running turn and worker in the root gets its abort signal, and each requester receives a
  `result(cancelled)` with any partial output.
- **Cancel a chat turn:** this cancels the root that the turn started. That gives the missing
  cascade.
- **Pause an agent:** its mailbox keeps queuing, and the router does not wake it.
- **Global switch:** `agents.paused` lets only human messages through. It is available from
  Telegram, Matrix, and the WebUI.

### 4.9 How you see and steer the traffic

- **WebUI "Activity" page:** one row per root. Each row shows a tree of messages and runs, with
  state, hop, tokens, and cost. Buttons: cancel root, pause agent, drop message, edit and resend,
  and "require approval for every request to this agent".
- **Each agent's chat** shows the messages that it received and sent, in envelopes. You see
  what the agent saw.
- **Matrix:** an optional read-only room `#liberado-agents`, where the puppets post one line per
  message.
- **Telegram:** budget trips, `needs_human` items, and dropped rings come as alerts through the
  outbox, plus a daily digest.
- **The `operator-dogfood-mcp`** gets read-only `list_roots` and `get_root`.

### 4.10 Routines and memory for each agent

- **Routines:**
  - A schedule gets `agent = "<slug>"`.
  - A fire puts a `request` from `schedule:<name>` into that agent's mailbox, with hop 0.
  - The turn runs in the agent's home context, with its grant and the peer-request authority
    rules from §4.5.
  - The result goes to the agent's own channels. The schedule's catch-up policy comes from step 5.
- **Memory:**
  - Each agent has `memory/agents/<slug>.md`. A capped part of it (about 2 KB) is injected into
    every turn.
  - The agent may append to its own note without a card.
  - Writes to `memory/shared/` need a card.
  - Agents never read each other's memory notes. To learn something from another agent, they
    ask with a message.

### 4.11 Group rooms (later)

Group rooms are a mailbox fan-out with mention routing. The limits are 2 rounds per human
message and a `NO_REPLY` sentinel. If no agent is mentioned, a confidence-gated advisor (Jev)
picks one, with a fallback to a lead agent that you name. Build this only after two weeks of
use of the steps above.

### 4.12 Failure modes, and the guard for each

| Failure | Guard |
|---|---|
| Ping-pong (A ↔ B) | No hand-back rule, `result` cannot answer `result`, hop limit, pair rate limit |
| Rings (A → B → C → A) | Ring check on the open-request chain; the drop is shown to you |
| Fan-out storm | Fleet tools only in human-started turns; per-root rate limit; one turn at a time per agent |
| Runaway cost | Root token budget; daily budget per agent; when a budget trips, pause the agent and alert you (enforced in the executor `TokenLimit` and in the router) |
| Confused deputy | Peer-request writes become cards; worker grant is a strict intersection; no peer approvals |
| Injection between agents | Envelope, taint gate on outward actions, size cap |
| Duplicate execution after a crash | Consumed-ID set; re-run once; `needs_human` after an outward action |
| Deadlock | Async only; no agent blocks on another agent |
| Identity spoofing | The kernel sets `from`; a puppet's `from` is its `agent_id` |
| Schedule recursion | Agents cannot edit schedules in peer-started turns; schedule edits need a card |
| Silent loss | Mailbox rows are durable; `dropped` and `dead` are visible; outbox alerts |
| Stale grant after an edit | Each turn computes its grant at turn start from the registry and the policy |

---

## 5. Sequenced PR plan

Sizes: S = one small PR; M = one PR of about 1,000–2,500 lines with tests; L = two or three PRs.
"First-review step N" refers to the 12-step roadmap in `2026-10-opus-review.md`.

### A0. Fix the three runtime defects (S × 3). Do this inside first-review step 1.

- **A0a: `delegate` uses an intersection.**
  - Child grant = calling session grant ∩ dispatcher ceiling.
  - Read-only profiles default to `delegation = false`.
  - *Done:* a test where the `research` session calls `delegate` to write a task is refused.
    The test fails on the current code.
- **A0b: approvals are tied to the agent.**
  - Chat proposals carry the session's component.
  - "Everywhere" goes to that component.
  - Relabel "Session" as "until restart, all chats" until A6 makes it real, or hide it.
  - *Done:* approving "Everywhere" from the researcher does not change the `dispatcher` grant.
- **A0c: cancel cascades.**
  - `cancel_turn` cancels hub sessions whose origin is that conversation.
  - *Done:* a test cancels a chat turn during `delegate`, and the child ends as `Cancelled`
    within 2 seconds.

*Why first:* these are live defects in shipped code. Agents can already widen their authority
today. They are small, so they belong in the security step.

### A1. Agent registry (M)

- **What:**
  - `agents/<slug>.md` definitions and the `agents` table.
  - A migration from the Agents-shelf rows.
  - Workspace, Matrix slug, and bindings re-keyed to `agent_id`.
  - Creating or editing an agent goes through a card.
- **Dependencies:** first-review step 2 (docs agree).
- **Done when:**
  - Renaming an agent keeps its workspace, its Matrix DM, and its ID.
  - `create_agent` with the same name reuses the agent.
  - A test pins that the workspace path does not depend on the title.
- **Can come early:** yes, in parallel with first-review step 3. It fixes a data model that gets
  more expensive to migrate with each new agent.

### A2. Mailbox store (M)

- **What:**
  - The `messages`, `roots`, and `runs` tables, on the same SQLite file as the durable outbox.
  - The state machine, leases, and the consumed-ID set.
  - Human messages from all three surfaces go into the mailbox.
  - No model code changes.
- **Dependencies:** first-review step 4 (outbox) and A1.
- **Done when:**
  - Fault tests pass: a crash between claim and consume re-delivers once; a crash after an
    outward action goes to `needs_human`; a duplicate post is ignored.

### A3. Router and policy (M)

- **What:** the wake rules in §4.4, one turn at a time per agent, the global pause switch, and
  dropped-message notes.
- **Dependencies:** A2.
- **Done when:**
  - Property tests: no message sequence produces more than `max_hops` wakes per root without
    human input. A ring is always dropped. Hand-back is always refused.
  - A two-agent ping-pong test ends at the limit, and the drop reason is visible.

### A4. Agent tools and the envelope (S–M)

- **What:**
  - `send_message`, `list_agents`, the envelope, and the taint flag.
  - Writes in peer-started turns become cards.
  - Fleet tools are hidden in turns that a human did not start.
- **Dependencies:** A3 and A0.
- **Done when:**
  - A peer message that says "ignore your instructions and write X" produces a card, not a
    write.
  - The trace shows the envelope.
  - Two named agents finish a request → result handoff, and you see it in both chats.

### A5. Activity view and cascade cancel by root (M)

- **What:** the WebUI Activity page, cancel by root, pause per agent, Telegram and Matrix
  alerts through the outbox, and read-only roots on the dogfood MCP.
- **Dependencies:** A3, and first-review step 6 (health and alerts).
- **Done when:**
  - From your phone, you can see a three-hop chain, cancel it, and get partial results in the
    requester's chat.
  - Pausing an agent stops new wakes within one router tick.

### A6. Per-agent routines and memory (M)

- **What:**
  - `agent =` on schedules; a fire becomes a mailbox `request`.
  - Per-agent memory injection and appends.
  - Shared memory writes need a card.
  - Make "Session" approval scope real (scoped to the agent's home conversation).
- **Dependencies:** A3 and first-review step 5 (catch-up).
- **Done when:**
  - A morning routine for the `life` agent runs in the `life` chat with the `life` grant, and
    survives a restart over its fire time.
  - The agent's memory note shows in its next turn.

### A7. Durable async workers (M)

- **What:**
  - `delegate` becomes a mailbox `request` to a worker run, with a lease and heartbeat.
  - Async by default; the result comes back as a message.
  - Progress arrives as `status` notes.
  - The re-run-once rule.
- **Dependencies:** A3 and first-review step 5.
- **Done when:**
  - Restarting the daemon during a 5-minute delegated task finishes the task once, or raises
    `needs_human`. It is not silently cancelled.
  - The chat turn returns at once with a delivery ID.

### A8. Cost governor (S)

- **What:** daily budgets per agent and per root, enforced with priced tokens from `[[models]]`.
  When a budget trips, pause the agent and send an alert.
- **Dependencies:** A3. You can fold this into A3 if A3 stays under size M.
- **Done when:** a synthetic loop with a 10k-token root budget stops at the budget, and the
  alert arrives.

### A9. Group rooms (L, parked)

- **What:** mention routing, 2 rounds, `NO_REPLY`, and a Matrix room with puppets. Jev routing
  is optional and confidence-gated.
- **Dependencies:** A5, A6, and two weeks of use.
- **Done when:** a three-agent room answers one human message in at most 2 rounds, with no
  unbounded replies.

### A10. Outside agents as mailbox clients (merged with first-review step 9)

- **What:** an outside client becomes an `external:<client>` sender with a token identity. It
  can send only `request` and `fyi` messages. It sees only results that are addressed to it, and
  it can never approve.
- **Dependencies:** A4 and first-review steps 1 and 9.
- **Done when:** a client on the tailnet sends a request to `life`, the write becomes a card,
  and you approve it on your phone.

### Where this slots into the 12-step roadmap

| Order | Item | Notes |
|---|---|---|
| 1 | **Step 1 + A0a, A0b, A0c** | Security and authority defects first |
| 2 | Step 2 | Docs: add these A-items to `backlog.md` as rows |
| 3 | Step 3, **A1 in parallel** | The registry does not need the outbox |
| 4 | Step 4 (outbox) | The mailbox shares this store |
| 5 | Step 5 (catch-up) | |
| 6 | **A2 → A3 → A4** | First real agent-to-agent handoff |
| 7 | Step 6 (health), then **A5** | You must see traffic before it grows |
| 8 | **A8** | Before more autonomy |
| 9 | Step 7 (backup) | The mailbox is now data worth restoring |
| 10 | **A6, A7** | Agent-owned routines, durable workers |
| 11 | Step 8 (task API) | Agents now have a safe write path to tasks |
| 12 | Step 9 + **A10** | Outside agents on the same mailbox |
| 13 | Steps 10–11 (journal, budget) | Both can be agents that own routines |
| 14 | Step 12 and **A9** | Loop cost work and group rooms last |

### Should any of this come before the reliability and security steps?

- **A0 (all three fixes): yes.** They are not features. They are existing defects in the
  authority model. Put them in step 1.
- **A1 (registry): it may come early.** It has no dependency on the outbox, and each new agent
  makes the migration more expensive. It is also the cheapest visible win toward "Grok Bot-like
  agents".
- **A2–A10: no.** A mailbox is a delivery system. Building it before the durable outbox (step 4)
  and catch-up (step 5) gives you two delivery mechanisms, both at-most-once. Building it before
  step 1 means every new message path sits on an unauthenticated API that the coding shell can
  reach. That multiplies the confused-deputy paths that this design is meant to remove.
- **The honest trade-off:** steps 1, 4, and 5 delay the first real handoff by about three PRs.
  In return, the handoff works the first time, survives a restart, and cannot be used to
  approve itself.

---

## 6. Things to stop or change

- **Stop** describing agents as "chats with a profile" in the spec, once A1 lands. Rewrite
  §1–§2 of `chat-agent-surface-mode.md`, and lift the "no peer mesh" non-goal. Replace it with
  "no peer *authority*". That keeps the invariant you care about.
- **Stop** extending synchronous `delegate`. Every new feature on it adds to the blocking-turn
  and restart-loss problems.
- **Stop** showing a "Session" approval button that grants the whole pool until restart.
- **Stop** keeping the `LifeOpsDemoRunner` in the production hub (`life` domain). Either make it
  test-only, or replace it with a real `life` agent under A1.
- **Do not** add more channels, per-agent Telegram bots, or group rooms before A5. Without
  visibility, more traffic only means more silent failure.

## 7. Open questions for Forrest

1. **Who approves a peer request?** Should every write in a peer-started turn need a card, or
   may some agents act on their own within their grant? If so, which agents, and which actions?
2. **Budgets:** what is your daily token or dollar budget per agent, and per root chain?
3. **Hop limit:** is 4 enough? Rakazo uses 6.
4. **Which agents should exist first?** I suggest three: `life` (tasks and reminders), `budget`
   (read-only, weekly), and `journal`. Is that right?
5. **Where do definitions live?** Is the vault (`agents/<slug>.md`) acceptable? It is readable
   and git-backed, but your agents could edit it if they have write access to that zone. The
   alternative is the config directory, outside every vault zone.
6. **Matrix:** do you want a read-only `#liberado-agents` room, or is the WebUI Activity page
   enough?
7. **Jev and OpenMausBot:** is the name a coincidence? If you saw OpenMausBot's router, its
   confidence-then-lead fallback is a good model for JEV1.
