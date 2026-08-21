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

1. Autonomous life-OS daemon.
2. Lean chat surfaces.
3. Coding pack with the best accepted result per dollar.

The reason for this order is in [`positioning.md`](spec/architecture/positioning.md).

## Priority 1 — autonomous life-OS daemon

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

## Priority 3 — coding pack

The target is a merge-ready result under a fixed task, repository commit, model, provider, and
resource budget. Tool style and turn count are diagnostic measures, not the product result.

Do this in order:

1. Publish the controlled cross-harness baseline described by backlog item 0.7 / C3.
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
Use [`harness-comparisons.md`](spec/reference/harness-comparisons.md) for the controlled-run contract.

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
- Tier 2 model-in-the-loop conformance, unless a change cannot be verified deterministically.

The active implementation order is always the
[`backlog`](future-work/backlog.md#implementation-order).
