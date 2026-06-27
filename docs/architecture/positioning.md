# Positioning

Liberado is **not** trying to win the agent-framework market. The goal is narrower and more honest:
**build something objectively more useful *for its author* than the existing free alternatives**
(OpenClaw/Clawdbot, Hermes, LibreChat). Building from scratch is only justified where those tools
fall short on the metrics that matter here — otherwise we would just use them. They are also valuable
*reference designs*: studying them tells us what is useful and what is not, and adopting their good
parts is smart engineering, not imitation. We compete only on the metrics the author cares about, not
on market share.

## The wedge — self-improving autonomy with guarantees

The three reference systems expose a gap between *power* and *trust*:

- **OpenClaw / Clawdbot** (TypeScript CLI + gateway; ReAct loop; on-demand "skill" loading;
  ~5,700-skill ClawHub; 15+ messaging channels) — broad and powerful but **structurally insecure**:
  ~470 documented advisories and ~17% defense rate against sandbox escape (security bolted on, not
  engineered).
- **Hermes** (Python; self-improving) — the closest in spirit and **currently ahead on shipped
  capability**: runtime self-improving skills (DSPy/GEPA auto-optimization), built-in cron, six
  execution backends with serverless hibernation, subagents. But it self-improves by running
  **un-contained Python**; safety is emergent/prompt-level.
- **LibreChat** (Node.js; Mongo + Meilisearch + Postgres/pgvector + a RAG-API container) — safe and
  polished, first-class MCP, strong multi-user/enterprise UI — but a **heavyweight chat UI, not an
  autonomous agent**, and operationally too heavy to justify for a single user.

None of them can claim that *agent self-extension cannot widen authority* — because none has a hard
capability boundary the LLM can only narrow. Liberado can: a **Rust-native, memory-safe core where
the LLM proposes and deterministic code disposes**, where self-improvement happens via **`ProposeMcp`
-> a Rust/WASM-sandboxed MCP -> capability-gated hot-reload** rather than arbitrary code execution,
where the **dispatcher-as-tool-advisor** keeps context lean, and where the **MCP/ACP-first
event-mesh** makes the whole thing modular and partially deployable.

## What we deliberately do NOT chase

OpenClaw's integration breadth and skills-catalog scale, and LibreChat's enterprise multi-user UI. We
win on **provable containment + lean context + single-binary autonomy** — the metrics that matter for
this project.

## Honest caveat

Hermes is genuinely ahead on shipped capability today. The thesis only holds once Liberado closes its
four gaps (self-improvement, cron, execution environments, subagents) — but it closes them *inside the
capability model*, which is the one thing no competitor can retrofit cheaply.

The self-improvement *engine* is not hypothetical: a tested Rust implementation (`riggers/`, an MCP
"PR factory" that turns a plain-English task into a draft PR behind human approval) already exists and
slots in as an MCP (see the [roadmap](../roadmap/current.md)). It is homelab-PR-shaped today and still
needs the integration steps there, but it is concrete evidence the moat is buildable rather than only
planned.

## Patterns worth stealing (smart engineering)

- **On-demand / lazy tool loading** (OpenClaw + Hermes) — validates the dispatcher-as-tool-advisor;
  keep a compact catalog, load full tool detail only when routed.
- **Self-improvement as a measured loop** (Hermes) — but via `ProposeMcp` + capability gate, not
  code-eval. Hermes' DSPy/GEPA auto-optimization of *tool descriptions* is worth watching — it
  directly improves a tool-advisor.
- **Execution-environment abstraction with serverless hibernation** (Hermes) — an
  `ExecutionEnvironment` trait for cheap always-on agents.
- **Cron as just-another-event-source** (Hermes + meshify) — near-free once the event-bus exists.
- **Persistent human-readable Markdown memory layers** (OpenClaw) — fits the vault model.
- **Single-port daemon/gateway control plane** (OpenClaw) — validates daemon-first.

*Competitive grounding gathered 2026-06-26 (web research; grokipedia sources were unavailable). See
[`docs/ideas/vs-hermes.md`](../ideas/vs-hermes.md) for the detailed Hermes gap analysis.*
