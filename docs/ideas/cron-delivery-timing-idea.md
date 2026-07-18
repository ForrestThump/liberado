# Cron delivery timing — quiet-delay append (built) + reply-to threading (deferred)

**Status:** 2026-07-18. Quiet-delay append is **implemented + live-verified** (a fired brief delivered
immediately with no chat activity and appended into the sticky "Telegram" conversation); reply-to
threading is a **deferred** follow-on.

> **Resolved 2026-07-18 — the sticky id now persists across restarts** (`server/src/sticky.rs`,
> `StickySession`): written to `<data_dir>/telegram-sticky-session` and restored on boot (a restored id
> is adopted only if its conversation still exists, else discarded). A container restart no longer opens
> a fresh "Telegram" conversation. Live-verified: brief → pointer on disk → restart → `restored sticky
> Telegram session from disk` → post-restart brief appended to the same conversation. Related: the cron→telegram delivery
(`daemon::maybe_deliver_cron_result`) and the sticky Telegram chat surface (`server/src/telegram.rs`),
the P1 automation story in [`../roadmap/current.md`](../roadmap/current.md) (C1), and E5-b (answer a
session from your phone).

## As implemented

- **`Notifier::deliver_cron`** (crates/notify) — a new trait method distinct from `notify`. Default is
  a plain immediate `notify`, so every other channel and all tests are unchanged; only a chat-aware
  channel overrides it. The daemon's `maybe_deliver_cron_result` now calls `deliver_cron`.
- **`ChatDeliveringNotifier`** (crates/server/src/cron_delivery.rs) — the override. `deliver_cron`
  waits for quiet, `append_note`s the brief into the sticky Telegram session, then pushes via its
  inner `TelegramNotifier`. `notify`/`notify_proposal` pass straight through (proposals stay
  immediate + button-based). Pure `next_wait` decides the delay from the activity clock + config, so
  the timing is unit-tested without real time.
- **Shared state** wired in `server::run`: an `Arc<Mutex<Option<Ulid>>>` sticky session id (bridge +
  notifier) and an `Arc<Mutex<Option<Instant>>>` last-activity clock (approval bot stamps it on every
  inbound message; notifier reads it). Both point at the same instances, so a brief appends into the
  exact conversation a reply continues.
- **Config** `[tuning.cron_delivery]`: `quiet_delay_secs` (default 300) and `deliver_by_secs` (default
  2700). Global for now; per-schedule override is a later refinement.
- Active only when both Telegram and a chat surface are configured; otherwise the daemon keeps the
  plain immediate notifier (unchanged behaviour).

## The problem

A cron brief is delivered to Telegram by the daemon via a one-way `Notifier.notify()` from the `cron:`
**goal session**. Incoming Telegram messages are handled separately and routed to the sticky
**chat session** (`TelegramChatBridge`, reset only by `/new`). The two never meet, so **replying to a
brief has none of the brief in context** — it reads as fresh context relative to the brief. And a
brief pushed while you're mid-conversation interrupts and pollutes that thread.

Three goals, in tension: **continuity** (a reply sees the brief), **non-interruption** (a brief
doesn't barge into an active chat), **timeliness** (the brief still arrives reasonably on time).

## Built: quiet-delay append into the sticky session

Fold the brief into the sticky Telegram chat conversation via `ChatSessions::append_note` — the exact
primitive built for goal-session return handoffs (a specialist session's summary folded back into its
parent chat as an assistant-role note). A cron brief is the same shape; it just has no parent
conversation, so it appends to the sticky Telegram session specifically. Then a reply runs `turn()`
over the full history *including* the brief → continuity, both directions (the brief sees the ongoing
chat context too).

Timing, so it doesn't interrupt:
- **Defer append *and* push together.** The brief enters the session at the moment it's pushed, so the
  order matches what you see; nothing is silently injected mid-conversation.
- **Common case is instant.** The delay only triggers when you're *actively* chatting (recent inbound
  message / a turn in flight). A 6:55am brief while you're asleep → last-activity is stale → delivered
  immediately. Normal timeliness is preserved; the timer only bites mid-conversation.
- **Quiet delay:** deliver after the chat has been inactive for `quiet_delay` (~5 min default).
- **Max-defer cap:** deliver when quiet *or* when `deliver_by` past schedule hits (~45 min default),
  whichever first — so a brief is never indefinitely late even if you chat straight through.
- The **cron still runs on time**: the session executes on schedule and is stored/joinable
  immediately; only the *delivery* to your phone defers. Deferring costs nothing on the execution side.

"Active" = last *inbound* (your) message, plus "no turn in flight." The brief's own send and the
assistant's typing must not count as activity, or the timer never fires.

Config (v1 global; per-schedule override is a later refinement): `quiet_delay_secs`, `deliver_by_secs`.

## Deferred: reply-to threading

Telegram messages carry ids and the bot already handles `reply_to_message` (proposal revisions use
it). One *could* let a native reply-gesture on a brief pin a followup to that specific brief.

**Why it's deferred (and why quiet-delay makes it largely redundant):**
- With quiet-delay append, by the time you'd reply the brief is **already in the one sticky session**,
  so a plain typed followup already carries it. Reply-to would only add pinning to a specific *old*
  brief far up the scrollback.
- If a reply-to routed to a *separate* Liberado session, it would split that exchange's context off
  from the main thread — and **that boundary is invisible in the Telegram app** (Telegram shows one
  linear thread; Liberado would be reasoning over fragments). That mismatch between the visible thread
  and the actual context is the real hazard, and a good reason not to build it casually.
- Precise reference to an arbitrary older conversation is exactly what the **mature WebUI mobile
  interface** is for (E5-b / W1) — the right home for "open any conversation and continue it," rather
  than overloading Telegram's flat thread.

Net: **Telegram display ≠ Liberado context.** Quiet-delay append keeps the two aligned (one thread,
one session); reply-to threading risks un-aligning them for a need the WebUI will serve better.
