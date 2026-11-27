---
kind: plan
status: active
authority: implementation
domain: product
canonical_for: product-roadmap
open_items: true
---

# Liberado — roadmap

One file holds both things: where the product is going, and what to build next.

- **Direction** is below, in the first section.
- **The work queue** is the **Now / Next / Later** sections. Everything a person or an agent
  implements comes from here. There is no second queue.
- **Done (recent)** and **Dropped** record why work is not open, so the same item is not restarted.

The old queues are history, not truth:
[future-work/archive/backlog.md](future-work/archive/backlog.md) and
[future-work/archive/direction.md](future-work/archive/direction.md). The October 2026 reviews that
set that order are in [reviews/](reviews/).

**Last edited 2026-11-26.** The 2026-10-18 and 2026-11-01 decisions still hold where this file does
not supersede them.

---

## Direction

### Mission

Own the personal infrastructure; rent the intelligence economically (whether through an API or
proprietary interface like Grok Bot, and through a flat rate subscription with limits or a per
token basis). Liberado is a user-owned information space and single source of truth for notes,
tasks, reminders, budget, calories, plans, schedules and routines. No vendor lock-in.

### Why

- Hosted assistants (Grok Bot, ChatGPT, etc.) already out-polish any UI we can build, for about the
  cost of the inference needed to run Liberado's own agent loop.
- The durable value is the data, the rules and reliable delivery, not the chat interface.

### Layers

1. **Substrate** (primary): data, schedules, reminders, budget, vault, approvals, backups,
   recovery. Deterministic and code-owned.
2. **Access**: first-class, easy-to-connect clients. Grok Bot and other agents read and write
   through scoped MCP/API access with approval gates enforced in code.
3. **Interfaces** (secondary): WebUI, Telegram, Matrix, plus an optional native agent loop over
   rented API inference. Good enough, not a race.

### Principles

- Durable state over durable conversations.
- Deterministic code for authorization, scheduling and fire times; models only interpret.
- Provider independence: any model or agent service can be swapped.
- Integrate before reimplementing (TurboVault, Obsidian Tasks, Actual).
- Least privilege; writes approval-gated with backup and validation.
- Readable by humans and agents (plain-English schedules, code-computed next fire times).
- **Polish small pieces fully before adding surface.** One active implementation track at a time. A
  slice is done only when Forrest uses it daily and would notice if it broke.

### Decision filter

Would this still be valuable if the owner's preferred hosted assistant were already excellent at
reasoning, planning, coding and tool use? If no, deprioritize.

**Scope smell:** designing multi-user accounts or account-owned devices for a one-person daemon
means the chunk grew past "small polish."

### Non-goals

- A UI feature race with commercial assistants.
- A general-purpose assistant competitor.
- Only an optimal coordination substrate is deferred until the personal basics work. Specialist
  agents with a durable mailbox are in scope.
- **CalDAV as a notification bootstrap** (tasks only, not approvals or cron or agent questions;
  bidirectional vault sync is a rabbit hole; extra server and auth; task apps cannot show Liberado
  context).
- **A native Android app** for reminders (PWA Web Push is first-class on Android via Chrome/FCM; an
  app does not fix thin notification copy and adds build, client, and FCM key work).
- **Account-owned devices** (the planned "PR3": account ownership of devices, notifications,
  conversations and reminders). Dropped 2026-11-01. Liberado is a single-user daemon: a Web Push
  subscription is the device identity, and the service worker acks outbox rows with per-row
  `{id, token}` tokens, not with accounts.

### Decisions that still hold

**2026-10-18.** First substrate priorities: tasks and reminders, then budget, plus an interactive
journal (capture thoughts in Obsidian daily notes, talk to an agent about them, and keep the
conversation and a parallel artifact linked in both Liberado and Obsidian). Mostly polish and use of
what already exists. Outside-agent access: a public endpoint is wanted only if it can be made
extremely secure, because the data is very personal; the default stays Tailscale-only until a
hardened design is reviewed. Jev: a working, unpolished, mergeable prototype, off by default and
toggled in config — lower priority than the substrate work.

**2026-11-01.** Accounts scope closes with PR #332. Remote access extends Tailscale / the Track B
daemon tunnel and remote attach before layering more application auth.

**2026-11-26.** One file holds direction and the queue. The active tracks are tasks/reminders and
chat stream reliability (see Now). `ntfy` and nightly mutation tests are dropped (see Dropped).

### Open questions

- Hardened outside-agent access design.

---

## Now

Forrest set these on 2026-11-26. Do not add other tracks here.

### Track 1 — Tasks and reminders

In this order:

1. **Land the TurboVault fix.** Merge the fork pull request
   [ForrestThump/turbovault#7](https://github.com/ForrestThump/turbovault/pull/7) — *"fix(tasks):
   keep #task lines, nested bullets, and clock times from breaking the parser"*. This lands
   upstream, in the TurboVault fork, not in this repository.
2. **Bump the Liberado pin.** After that merge, point the `turbovault*` git dependencies in this
   workspace at the merge commit and update `Cargo.lock`. Cargo fetches the forks by revision, so a
   pin bump is a manifest and lockfile change.
3. **Task digest on TurboVault parsing.** A digest over the parsed tasks, with the `#task` global
   filter applied.

### Track 2 — Chat stream reliability

- Reconnect with a history refetch.
- `/attach` backoff.
- A "Connection lost" banner with a **Retry** button.
- **No page-reload band-aid.** A reload is not a reconnect.

---

## Next

Forrest placed these here.

### Product and interface

- **WebUI jump-to-bottom button.**

### Small ops items (from Liberado Maintainer)

- `topology.toml` permissions: `chmod 600`.
- Backup pruning.
- Schedule-writer backups.
- A boot topology snapshot.
- The flaky test `execute_direct_building_line_is_emitted_at_info_level`
  ([crates/orchestrator/src/lib.rs](../crates/orchestrator/src/lib.rs)).
- `query_frontmatter_sql` fails with *"SQL feature not enabled"*.
- `.trash/` is indexed. The reminder vault walk already skips `.trash`
  ([crates/daemon/src/vault_pings/walk.rs](../crates/daemon/src/vault_pings/walk.rs)); the
  TurboVault index behaviour was not verified.
- A detached `~/homelab/liberado` checkout makes `setup.sh` exit 1.

### Quality-gate gaps (open code items)

- **CRAP and cyclomatic no-regress vs merge-base: already shipped** (`liberado ci crap` and the CI
  `crap` job). Not a gap; recorded here so the list is complete.
- **Module health should not get worse vs the branch's merge-base.** This is the gap. Today module
  health compares against the committed
  [`code-metrics/module-health-baseline.json`](../code-metrics/module-health-baseline.json), allows
  growth up to the review boundary (`ploc_review = 1000`), and Linux `just ci` may ratchet the
  baseline file. There is no merge-base comparison, unlike the function gate.
- **A coverage floor: not shipped.** `llvm-cov` is used only to compute CRAP for touched functions.

---

## Later

Items Forrest does not remember, which have not caused visible pain. The rationale, the
done-when text, and the dependency order live in the
[archived backlog](future-work/archive/backlog.md) and the October 2026 reviews
([2026-10-opus-review.md](reviews/2026-10-opus-review.md),
[2026-10-opus-agent-runtime.md](reviews/2026-10-opus-agent-runtime.md)). This section lists them;
it does not restate them.

- **Step 4 leftover.** Approval cards — and the weekly spending push — do not go through the durable
  outbox row machine. Reminders already do (PR #329). No `ntfy`; see Dropped.
- **Step 5 leftovers.** There is no catch-up policy `all`. Scheduled goals are not `direct = true` by
  default. "Index the vault by changed files only" was not found.
- **Step 6.** `liberado doctor`, `/api/health/ready`, and a dead-man alert. Only the `GET
  /api/health` liveness endpoint exists.
- **Step 7.** `liberado backup`, `liberado restore`, `liberado export`.
- **Step 8, the parts Now does not cover.** The tasks module, the WebUI task list, stable
  `^lib-xxxx` IDs, and the Telegram `/remind`, `/done` and `/snooze` commands.
- **Step 9 and A10.** A streamable-HTTP MCP server with per-client tokens and scopes, and outside
  agents as mailbox clients.
- **Step 10.** The interactive journal.
- **Step 11 leftovers, after PR #317.** The hand-written Web Push (a maintained crate is still an
  open option; `ntfy` is Dropped). The weekly send bypasses the outbox worker. The outside-agent
  budget tools depend on Step 9.
- **Step 12.** A cheaper, simpler native loop (fallback only).
- **Agent-runtime rows.** A1 to A8 (A6 and A7 included), and the parked A9 group-rooms/Jev row.
- **The frozen coding pack.** R1, C3, C5, C4; TE1 and TE2; the later E2, E4 and E5; the deferred
  C6; and the coding-worker control plane.
- **Shorten AGENTS.md.** It is still about 260 lines.
- **`liberado-policy` ([crates/policy](../crates/policy)).** Unused: no Rust crate depends on it, and
  its rustdoc says the runtime does not call it yet. Later item: wire it, document it as unwired,
  or remove it. This is a statement of fact, not a redesign.
- **CATALOG frontmatter for the core spec pages.** An open question, not a decision.
- **The old Step 2 mechanical docs-audit idea** — "fail if `roadmap.md` names an order that
  `direction.md` does not" — is moot, because direction is no longer a second queue. It is not to
  be built.
- **Working proposal, not current authority:**
  [future-work/2026-11-20-architecture-direction-mcp-handoff-brief.md](future-work/2026-11-20-architecture-direction-mcp-handoff-brief.md).

---

## Done (recent)

- **Accounts + passkey.** PR #332, merged 2026-11-01. The last accounts PR.
- **Notifications track N1–N3.** PR #334 (rich payload and `/notifications/<id>`) and PR #335
  (Done/Snooze on the system notification). Forrest: complete for now.
- **A0, runtime authority.** PR #319, 2026-10-25. Tests exist for A0a, A0b and A0c.
- **Step 1, close the control plane.** PR #320 and the follow-up PR #322.

Related landings that are not the current queue. Their leftovers are in Later:

- **PR #329** — outbox and catch-up for mechanical reminders.
- **PR #317** — the weekly spending Web Push.
- **PR #318** — the docs alignment prose.

---

## Dropped

Clearly superseded. The rationale stays linked; it is not deleted.

- **ntfy.** Forrest, 2026-11-26: not needed; Web Push works. The old mentions live in the
  [archived backlog](future-work/archive/backlog.md) and
  [2026-10-opus-review.md](reviews/2026-10-opus-review.md).
- **Nightly mutation tests.** Forrest: no. Mutants stay a local `just mutants*` campaign, with the
  ledger at [`code-metrics/mutants-ledger.json`](../code-metrics/mutants-ledger.json).
- **PR3, account-owned devices.** Forrest, 2026-11-01. See the non-goals above.
- **CalDAV as a notification bootstrap, and a native Android reminder app.** Direction non-goals.
  They are now stated in this file, and they remain in the
  [archived direction.md](future-work/archive/direction.md).
- **"Matrix over Mattermost" and "merge commits only".** A 2026-11-26 code audit did not find
  pages for either on `main`, so nothing is linked here. Do not invent them.

---

## Related

- [docs/reviews/](reviews/) — the October 2026 reviews that set the order this file inherits.
- [docs/future-work/](future-work/README.md) — plans, ideas, and research. **Read the status
  header.** This file is the queue; that folder is design material.
- [docs/future-work/archive/](future-work/archive/README.md) — finished plans and closed audits.
- [docs/spec/architecture/overview.md](spec/architecture/overview.md) — how the system works today.
- [docs/spec/reference/doc-authority.md](spec/reference/doc-authority.md) — what each document may
  claim.
- **The CRAP and cyclomatic no-waiver policy**:
  [`code-metrics/function-complexity.toml`](../code-metrics/function-complexity.toml) and
  [`crates/cli/src/ci_cmd.rs`](../crates/cli/src/ci_cmd.rs). There is no committed per-function
  baseline and no waiver.
