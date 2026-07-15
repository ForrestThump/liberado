# Positioning

Liberado is **not** trying to win the agent-framework market. The goal is narrower and more honest:
**build something objectively more useful *for its author* than the existing free alternatives** — the
autonomous-daemon tools (OpenClaw/Clawdbot, Hermes), the chat frontends (LibreChat), and the coding
agents (Claude Code, Grok Build, Kilo/OpenCode). Building from scratch is only justified where those
tools fall short on the metrics that matter here — otherwise we would just use them. They are also
valuable *reference designs*: studying them tells us what is useful and what is not, and adopting their
good parts is smart engineering, not imitation. We compete only on the metrics the author cares about,
not on market share.

The end state is one system that stands in for all three categories — one daemon, one session model,
one capability boundary, one memory, spanning automation + chat + coding. But it is built in a
**deliberate priority order** (below), because for a solo project effort is the scarce resource, not
scope.

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
where the **dispatcher-as-tool-advisor** keeps context lean, and where the **MCP/hook-first
event architecture** (kernel + domain packs + stores + surfaces, a star around one daemon) makes
the whole thing modular and partially deployable.

## Replacement priority — what we build first, and why in this order

**1. The autonomous life-OS daemon first (replace OpenClaw / Hermes).** This is where the free tools
are weakest exactly where it matters most — OpenClaw is structurally insecure, Hermes self-improves by
running uncontained Python — and where Liberado already holds its strongest cards: TurboVault as the
life-system store, the capability boundary, one daemon on one event architecture. The capabilities
that define this category, in the author's own priority: **good crons, broad MCP connections, agent
interfacing (see and answer your autonomous agents), notifications, and life-system storage.** Storage
is done (TurboVault) and the substrate exists (cron event-source, Telegram notifier, capability
model); the near-term work is the **interfacing loop** — an agent that works while you are away, pings
you, and lets you answer from your phone — plus maturing crons and MCP breadth.

**2. A lean chat surface second (replace LibreChat).** LibreChat is good, but it is a heavyweight,
multi-container chat UI that is hard to justify for a single user. The goal is not to out-feature it —
it is to be a self-hosted, **single-binary chat surface that is yours**, provider-flexible, and light
enough to actually run. Gated on the WebUI maturing past its current state (a chat component, no
session view).

**3. Coding third, and explicitly "good enough + integrated" — not best-in-class.** The author does
**not** intend to replace Claude Code, Grok Build, or Kilo/OpenCode at coding, and Liberado should not
try. The coding pack's job is to be good enough that the *integration* wins for the author's own
workflow — a coding session is just another `Session` on the same daemon, under the same capability
model, joinable from the same surfaces — not to beat a dedicated coding agent on its own turf. Chasing
that would weaken the thing that actually makes Liberado worth building.

**Sequencing effort, not scope.** Building in this order only works because choosing to spend effort
on (1) does not foreclose (2) or (3). That is precisely what the CI-enforced modularity is *for*:
kernel + domain packs + stores + surfaces, layer rules that fail the build, one converged execution
engine, deduplicated seams. Coding is *already* a domain pack on a domain-neutral kernel; chat is
*already* a surface on the same session store; a capability added for the daemon is available to all
three by construction. The architecture is the thing that lets a solo project prioritize ruthlessly
and still credibly claim it can replace all three eventually — the modularity is not neatness, it is
the load-bearing enabler of this whole plan. See [`modularity.md`](modularity.md) and
[`contracts.md`](contracts.md) for the frozen seams, and `crates/test-support/tests/layer_rules.rs`
for the rules that enforce them.

## What we deliberately do NOT chase

OpenClaw's integration breadth and skills-catalog scale, and LibreChat's enterprise multi-user UI, and
Claude Code / Kilo's best-in-class coding depth. We win on **provable containment + lean context +
single-binary autonomy + one coherent system across all three categories** — the metrics that matter
for this project.

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
