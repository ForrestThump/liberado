---
kind: index
status: active
authority: advisory
domain: docs
---

# Research

Dated analyses, peer studies, and source conversations. Useful for context; **not** the living
roadmap and **not** a source of implementation work.

## Specs these notes feed

| Spec | Role | Status |
|------|------|--------|
| [cross-harness-baseline.md](../cross-harness-baseline.md) | C3 experiment: four-way published score | active, backlog item 1 |
| [coding-worker-control-plane.md](../coding-worker-control-plane.md) | Liberado operates coding agents; native loop is one worker | draft, not scheduled |

## Active research

| Doc | Role |
|-----|------|
| [agent_pools_research_results.md](agent_pools_research_results.md) | Four independent model passes on concurrent-agent architecture; evidence for the no-peer-coordination decision behind dispatcher/executor pools |
| [orchestration-report-applied.md](orchestration-report-applied.md) | 2026 orchestration survey applied to Liberado: where we already match, real gaps (checkpointing, executable verification), and the prompt-caching correction |
| [evals_implementation.md](evals_implementation.md) | A third-party eval-loop write-up **plus the 2026-08-03 decision**: no harness yet — the gate is a free oracle, which coding has and life-ops does not. What was built instead, and when to revisit |
| [research-prompt-concurrent-agent-pools.md](research-prompt-concurrent-agent-pools.md) | The research prompt that commissioned `agent_pools_research_results.md` (provenance and constraints) |

## Source conversations

| Doc | Role |
|-----|------|
| [agent-orchestration-idea-from-Sol.md](agent-orchestration-idea-from-Sol.md) | Provenance for the control-plane spec. Not a plan. |
| [bob-martin-critique.md](bob-martin-critique.md) | Provenance for the control-plane constraints. Not a cleanup epic. |

## Archive

| Doc | Role |
|-----|------|
| [archive/](archive/README.md) | Dated architecture analyses and peer MCP studies |

For what to build next: [backlog.md](../backlog.md).
For direction: [roadmap.md](../../roadmap.md).
For how the system works now: [architecture overview](../../spec/architecture/overview.md).
