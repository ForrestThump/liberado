---
kind: plan
status: active
authority: advisory
domain: product
canonical_for: product-roadmap
open_items: true
---

# Liberado — roadmap

This page explains the direction of open work. It does not record completed implementation.
Current behavior belongs in code, tests, Rustdoc, and [`spec/`](spec/). Git history preserves work
that has landed.

Agents must select implementation work from the ordered
[`future-work/backlog.md`](future-work/backlog.md), not from this page. Verify an item against current
code before implementation. Read [`failure-modes.md`](spec/architecture/failure-modes.md) before
changing safety, tests, configuration, or agent control flow.

The product order remains:

1. Autonomous Liberado daemon.
2. Lean chat surfaces.
3. Coding pack with the best accepted result per dollar.

The reason for this order is in [`positioning.md`](spec/architecture/positioning.md).

## Priority 1 — autonomous Liberado daemon

The near-term goal is a daemon that is useful enough to operate every day. Dogfood the existing
Telegram surface and fix observed friction before adding another broad surface.

Open outcomes:

- **Daily use:** lean on sticky Telegram chat and scheduled delivery to find real failures.
- **Inbox:** add positive directory enumeration to TurboVault, then implement the two capture
  surfaces in [`inbox-spec.md`](spec/inbox-spec.md).
- **Homelab diagnostics:** stop the TurboMCP SSE reconnect storm so useful failures remain visible.
- **Mobile session view:** add goal-session visibility to the WebUI when Telegram becomes too flat.
  Follow [`session-surface-contract.md`](spec/architecture/session-surface-contract.md).
- **TurboVault modules:** finish `vault_events` and upstream the reusable module changes. See the
  [`TurboVault integration roadmap`](future-work/turbovault-modules-integration-roadmap.md).
- **Remote access:** finish Track B (daemon tunnel / remote attach) without coupling remote
  transport to the ACP coding agent. Local ACP interactive coding — tools, durable sessions,
  and Paseo permission prompts — is dogfood-ready. See the
  [`Paseo integration roadmap`](future-work/paseo-liberado-integration-roadmap.md).

Use the [conformance runbook](impl/live-conformance.md) for deterministic and deployed-daemon
checks. Conformance operation is not an open roadmap item.

## Priority 1.5 — token economics

The measured priority is the orchestrator context sent on each hop. The dated evidence and caveats
are in [`token-economics-findings-2026-08.md`](future-work/token-economics-findings-2026-08.md).

Do this in order:

1. Deploy the existing instruments and read one day of data.
2. Identify why the tool catalogue remains broad.
3. Narrow only the path supported by the measurement.

Do not add tuning knobs before a measurement shows that a constant is wrong. A setting that parses
but is not consumed adds risk without adding control.

## Priority 2 — lean chat surfaces

Open outcomes:

- Improve WebUI history, navigation, and mobile usability.
- Keep every surface a client of daemon APIs. Do not move session or agent control flow into a UI.
- Revisit the context-compaction viewport only if the known persistence residual justifies the
  added model. The proposed design is in
  [`context-compaction-viewport-rearchitecture.md`](future-work/context-compaction-viewport-rearchitecture.md).

Per-conversation model selection and its compaction trigger are implemented. Their current contract
belongs in the architecture and configuration references, not in this roadmap.

### Near-term callout — chat | agent surface mode (Reading B)

The immediate product-surface bet is a soft **chat | agent** split: a long-lived specialist
chat (a Grok-Bot-style context with curated tools, never terminal) lives on the **agent** shelf;
an open-ended chat lives on the **chat** shelf. The stamp is **not** `goal.is_some()` — it is
the create-time signal (explicit `surface_mode`, or the named profile in a small `agent_profiles`
set). Goal sessions default to `chat` on the chat lens; the agent sense is the profile one.

Slice order, all under Priority 2:

1. **Wire stamp** (Slice 1) — `surface_mode: "chat" | "agent"` on `ConvHeader` /
   `ConversationHeader` / `SessionHeader`. Stamped at create, defaulted to `chat` on read for
   legacy rows, upgraded for legacy rows whose profile is in `agent_profiles`. **Done**
   ([#274](https://github.com/ForrestThump/liberado/pull/274)).
   Spec: [`chat-agent-surface-mode.md`](spec/architecture/chat-agent-surface-mode.md).
2. **WebUI shelves + create-path** (Slice 2) — shelves, **New Agent** create-with-grant,
   privileged face `create_agent` (gate A). **In flight**
   ([#276](https://github.com/ForrestThump/liberado/pull/276)).
3. **Chat-default tools** (Slice 3 / CAS3) — named `chat-default` profile, `chat-search` granted to
   `main-agent` as a commented `policy.toml` block. **Backlog** (still next after this).
4. **Dogfood + measure** (Slice 4 / CAS4) — one week of shelves + chat-default, tune `agent_profiles`
   from observed usage. **Backlog.** Jev still after CAS4.

Jev lands **after** Slice 4 lands and is dogfooded — explicitly out of scope for the
chat/agent PR set. The post-shelf phase plan (TypeSafe Jev first wedge, non-goals, and
kernel constraints) is [`jev-integration.md`](spec/architecture/jev-integration.md).
TUI parity (kind filter) is deferred until shelves are stable. The
[`tui-maturity-roadmap.md`](future-work/tui-maturity-roadmap.md) is updated to reflect the
deferral.

## Priority 3 — coding pack

The target is a merge-ready result under a fixed task, repository commit, model, provider, and
resource budget. Tool style and turn count are diagnostic measures, not the product result.

Before another review worker is enabled, prove the Codex-only daemon review path on one repository.
The proof must include a remote-only PR head, restart-safe publication, a clean review, and a
blocker that produces the checklist and draft conversion. The ordered acceptance work is backlog
item R1.

Do this in order:

1. Publish the controlled cross-harness baseline described by backlog item 0.7 / C3.
   Spec: [`cross-harness-baseline.md`](future-work/cross-harness-baseline.md).
2. Measure the completion gate off and on before changing its default.
3. Change one evidence-selected mechanism at a time.
4. Finish dedicated goal-view panes after measurement and unattended correctness work.

Report:

- Ship-gate and merge-ready rate.
- Total cost per accepted result, including retries and reviewers.
- Wall-clock p50 and p95 when the sample supports them.
- Human repair time or repair diff.
- Trace-linked failure class.

Read [`coder-harness-reliability-2026-08.md`](future-work/coder-harness-reliability-2026-08.md)
before proposing a coding-pack fix. It records failed hypotheses as well as successful repairs.
Use [`harness-comparisons.md`](spec/reference/harness-comparisons.md) for the controlled-run
contract. The experiment itself is [`cross-harness-baseline.md`](future-work/cross-harness-baseline.md).

### Goal, graph, loop, and surface order

- Make ordinary `/goal` completion repeatably trustworthy before adding scheduler complexity.
- Prove the existing isolated fan-out path through the real build and merge path before accepting
  a general work graph.
- Keep `/loop` as a product scheduler over ordinary goals, not as a coding-performance mechanism.
- Keep surfaces as clients of kernel and pack APIs.

## Cross-cutting direction

- Preserve the layer rules and shared-kernel boundary in
  [`modularity.md`](spec/architecture/modularity.md).
- Use the [Model View Log](spec/reference/model-view-log.md) and execution log as the common
  cross-harness evidence format.
- Audit external dependencies for unused entries, version drift, and compile-time cost when that
  work enters the backlog.

## Deliberately not scheduled

These documents preserve possible directions. They are not selectable work:

- Repository map and automatic context selection, until measurements show a context-selection
  failure rather than a file-discovery failure.
- [Per-model knob profiles](future-work/model-knob-profiles.md), until controlled runs justify them.
- [Cadence-triggered maintenance agents](future-work/cadence-triggered-maintenance-agents.md), until
  ordinary unattended goals are reliable.
- [Coding-worker control plane](future-work/coding-worker-control-plane.md), until C3 is published
  and that table shows a reason to run an external harness as a production worker.
- Tier 2 model-in-the-loop conformance, unless a change cannot be verified deterministically.

The active implementation order is always the
[`backlog`](future-work/backlog.md#implementation-order).
