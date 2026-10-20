---
kind: plan
status: active
authority: advisory
domain: product
canonical_for: product-direction
open_items: true
---

Last edited 2026-10-18 by Forrest

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

## Non-goals
- A UI feature race with commercial assistants.
- A general-purpose assistant competitor.
- Chasing an optimal coordination substrate before the personal basics work.

## Decision filter
Would this still be valuable if the owner's preferred hosted assistant were already excellent at reasoning, planning, coding and tool use? If no, deprioritize.

## Re-ranked roadmap (proposal)
Now: DST-safe structured schedules (#313, live migration before Nov 1); reminder and vault-task reliability; Automations page; MCP servers page (read-only first).
Next: scoped, documented access point for outside agents (Grok Bot first); budget and calorie tracking in the substrate; backups and restore drills; export of all data in plain files.
Later / parked: further WebUI polish (as Forrest asks), Jev model routing, adaptive or generated UI, extra chat features.
Fallback kept: native agent loop over rented inference.

## Decisions so far (Forrest, 2026-10-18)
- First substrate priorities: tasks and reminders, then budget, plus an interactive journal: capture thoughts in Obsidian daily notes, talk to an agent about them, and keep the conversation and a parallel artifact linked in both Liberado and Obsidian. Mostly polish and use of what already exists (Obsidian, Liberado, TurboVault).
- Outside-agent access: a public endpoint is wanted only if it can be made extremely secure, because the data is very personal. Default stays Tailscale-only until a hardened design is reviewed.
- Jev: build a working, unpolished, mergeable prototype, off by default and toggled in config, so the idea is locked in without over-developing it. Lower priority than the substrate work.

## Open questions
- Hardened outside-agent access design.
