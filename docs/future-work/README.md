# Future Work

Index of forward-looking work.

| Doc | Role |
|-----|------|
| **[roadmap.md](../roadmap.md)** | **Living scoreboard** — open work in priority order, recently landed |
| [live-conformance-suite.md](live-conformance-suite.md) | T1 reliability suite (L1–L11 landed; Tier 3 open — Tier 2 optional) |
| [live-conformance-tier3-build-spec.md](live-conformance-tier3-build-spec.md) | Build spec for Tier 3 — deliverables, safety envelope, per-path assertions |
| Active plans below | Still drive near-term work (status in each file header when set) |
| [archive/](archive/README.md) | Finished plans, closed audits — **not current truth** |

## Active plans (not archived)

| Plan | Domain |
|------|--------|
| [loops-plan.md](loops-plan.md) | Scheduled recurrence over goals |
| [chat-search-plan.md](chat-search-plan.md) | History search tiers |
| [context-compaction-plan.md](context-compaction-plan.md) | Chat context compaction (CH3 — Tier 1 landed; known residual documented) |
| [durable-chat-turns-plan.md](durable-chat-turns-plan.md) | A turn outlives the connection watching it (**built + verified live** 2026-08-02; all steps landed) |
| [backlog.md](backlog.md) | **Pick-from-here backlog** (2026-08-03): four bands — token economics, correctness gaps, agentic coding, low-risk breadth. Self-scoped work starts here; carries the verify-first and per-behaviour-mutation rules |
| [parallel-deliverables-2026-08-round-3.md](parallel-deliverables-2026-08-round-3.md) | **Round 3 — status + open work.** §1 done; §2 (subagent vs direct in the journal) is next and has one mandated approach with verified line numbers; §3 split into a delegable measurement and a reserved fix. Carries the R1–R8 pre-PR checklist |
| [parallel-deliverables-2026-08-round-2.md](parallel-deliverables-2026-08-round-2.md) | Round 2 — five specs, **all landed** (PRs #33–#37); kept for the R1–R5 rules and the review record |
| [parallel-deliverables-2026-08.md](parallel-deliverables-2026-08.md) | Round 1 — five specs, **all landed** (PRs #28–#32); kept for the acceptance-criteria style and the review record |
| [token-economics-findings-2026-08.md](token-economics-findings-2026-08.md) | **Finding + TE1–TE3**: 56% of all spend is the orchestrator's ~11k base re-sent per hop; face context is 4.5%. Roadmap P1.5 |
| [token-cost-accounting-plan.md](token-cost-accounting-plan.md) | Price the existing latency journal so design calls use numbers, not guesses (scoped) |
| [delegated-work-is-discarded-at-the-seam.md](delegated-work-is-discarded-at-the-seam.md) | **Finding**: `delegate` passes only a summary, so the face agent fabricates the specifics. Root cause found; **context-cost objection now measured away** (2026-08-02) |
| [build-locally-and-ship-the-artifact.md](build-locally-and-ship-the-artifact.md) | Compile on the dev machine, ship the binary (wanted; blocked on disk + a WSL distro) |
| [context-compaction-viewport-rearchitecture.md](context-compaction-viewport-rearchitecture.md) | CH3.1 proposed: side-summary + viewport (not shipped) |
| [coding-tui-plan.md](coding-tui-plan.md) | Agentic coding TUI: goal surface + kernel completion gate (S1–S7) |
| [rust-native-agentic-coder-plan.md](rust-native-agentic-coder-plan.md) | Coding pack roadmap |
| [tui-maturity-roadmap.md](tui-maturity-roadmap.md) | TUI surface |
| [turbovault-modules-integration-roadmap.md](turbovault-modules-integration-roadmap.md) | TurboVault plugins umbrella |
| [session-profiles-plan.md](session-profiles-plan.md) | Per-conversation tool authority (steps 1–6 landed on `feat/session-profiles`, parked) |
| [turbovault-vault-events-plugin-plan.md](turbovault-vault-events-plugin-plan.md) | vault_events module |
| [mcp-forge-backlog.md](mcp-forge-backlog.md) | MCP forge service |
| [mcp-suite-standardization.md](mcp-suite-standardization.md) | Peer MCP suite norms |
| [latency-and-routing-observability-plan.md](latency-and-routing-observability-plan.md) | Latency journal → policy |
| [heuristics-tuning-engine-plan.md](heuristics-tuning-engine-plan.md) | Heuristics tuner tooling |
| [coder-eval-curriculum.md](coder-eval-curriculum.md) | Coding quality instrument |
| [pr-dispatch-vtcode-no-write-finding.md](pr-dispatch-vtcode-no-write-finding.md) | Open PR-dispatch bug |

### Coding pack / TUI
- [`rust-native-agentic-coder-plan.md`](rust-native-agentic-coder-plan.md) — umbrella design
- [`coding-tui-plan.md`](coding-tui-plan.md) — surface + completion-gate slices S1–S7
- [`tui-maturity-roadmap.md`](tui-maturity-roadmap.md) — TUI maturity audit

Start every planning session at [roadmap.md](../roadmap.md).
