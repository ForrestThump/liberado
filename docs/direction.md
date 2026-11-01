---
kind: plan
status: active
authority: advisory
domain: product
canonical_for: product-direction
open_items: true
---

Last edited 2026-11-01. The 2026-10-18 substrate priorities stand where this file does not supersede them.

# Liberado direction

Status: a directional nudge, not a rigid guardrail.

## Mission
Own the personal infrastructure; rent the intelligence economically (whether through an API or proprietary interface like Grok Bot, and through a flat rate subscription with limits or a per token basis).
Liberado is a user-owned information space and single source of truth for notes, tasks, reminders, budget, calories, plans, schedules and routines. No vendor lock-in.

## Why
- Hosted assistants (Grok Bot, ChatGPT, etc.) already out-polish any UI we can build, for about the cost of the inference needed to run Liberado's own agent loop.
- The durable value is the data, the rules and reliable delivery, not the chat interface.

## Layers
1. Substrate (primary): data, schedules, reminders, budget, vault, approvals, backups, recovery. Deterministic and code-owned.
2. Access: first-class, easy-to-connect clients. Grok Bot and other agents read and write through scoped MCP/API access with approval gates enforced in code.
3. Interfaces (secondary): WebUI, Telegram, Matrix, plus an optional native agent loop over rented API inference. Good enough, not a race.

## Principles
- Durable state over durable conversations.
- Deterministic code for authorization, scheduling and fire times; models only interpret.
- Provider independence: any model or agent service can be swapped.
- Integrate before reimplementing (TurboVault, Obsidian Tasks, Actual).
- Least privilege; writes approval-gated with backup and validation.
- Readable by humans and agents (plain-English schedules, code-computed next fire times).
- **Polish small pieces fully before adding surface.** One active implementation track at a time. A slice is done only when Forrest uses it daily and would notice if it broke.

## Non-goals
- A UI feature race with commercial assistants.
- A general-purpose assistant competitor.
- Only an optimal coordination substrate is deferred until the personal basics work. Specialist agents with a durable mailbox are in scope.
- **CalDAV as a notification bootstrap** (tasks only, not approvals or cron or agent questions; bidirectional vault sync is a rabbit hole; extra server and auth; task apps cannot show Liberado context).
- **A native Android app** for reminders (PWA Web Push is first-class on Android via Chrome/FCM; an app does not fix thin notification copy and adds build, client, and FCM key work).

## Decision filter
Would this still be valuable if the owner's preferred hosted assistant were already excellent at reasoning, planning, coding and tool use? If no, deprioritize.

**Scope smell:** designing multi-user accounts or account-owned devices for a one-person daemon means the chunk grew past "small polish."

## Re-ranked roadmap (proposal)
**Now:** finish [#332](https://github.com/ForrestThump/liberado/pull/332) (accounts + passkey login — last accounts PR). Then the **notifications polish** track in [`future-work/backlog.md`](future-work/backlog.md): richer push payload, per-notification WebUI detail, then optional Done/Snooze on the system notification — each step its own PR, lived with on the Pixel before the next.

**Paused until that track is daily-driver:** the October 2026 substrate queue (control plane, mailbox, outside-agent access, and the rest). Remote reach for an already-running daemon stays **Track B** (Paseo tunnel / remote attach — see [`future-work/paseo-liberado-integration-roadmap.md`](future-work/paseo-liberado-integration-roadmap.md)); borrow auth from the network layer (Tailscale) before adding more app-level auth.

**Later / parked:** further WebUI polish beyond notifications, Jev model routing, adaptive or generated UI, extra chat features.
Fallback kept: native agent loop over rented inference.

## Decisions so far (Forrest, 2026-10-18)
- First substrate priorities: tasks and reminders, then budget, plus an interactive journal: capture thoughts in Obsidian daily notes, talk to an agent about them, and keep the conversation and a parallel artifact linked in both Liberado and Obsidian. Mostly polish and use of what already exists (Obsidian, Liberado, TurboVault).
- Outside-agent access: a public endpoint is wanted only if it can be made extremely secure, because the data is very personal. Default stays Tailscale-only until a hardened design is reviewed.
- Jev: build a working, unpolished, mergeable prototype, off by default and toggled in config, so the idea is locked in without over-developing it. Lower priority than the substrate work.

## Decisions so far (Forrest, 2026-11-01)
- **Accounts scope closes with #332.** The planned "PR3" (account ownership of devices, notifications, conversations, reminders) is **dropped**. Liberado is a single-user daemon: a Web Push subscription is the device identity; the service worker acks outbox rows with per-row `{id, token}` tokens, not with accounts.
- **Active track after #332:** notifications polish only, in order: (1) daemon push `title`/`body`/`url` that answer "what is this and why now" on the lock screen, with `url` pointing at a detail route (backward compatible — `sw.js` already passes the full JSON payload as `notification.data`); (2) WebUI detail at `/notifications/<id>` (new route variant and component — the sidebar `Route::Notifications` page from #331 is settings and subscriptions only, not per-row detail); (3) later, Done/Snooze on the notification itself in `notificationclick`, only after the detail page is good.
- **Not doing now:** CalDAV notification bootstrap; native Android reminder app (see non-goals).
- **Remote access:** extend Tailscale / Track B daemon tunnel and remote attach before layering more application auth.

## Open questions
- Hardened outside-agent access design.
