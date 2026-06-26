# vs-hermes.md — Liberado vs Hermes Agent Competitive Gap Analysis

> **Purpose**: Identify high-leverage gaps where Hermes has features or architectural advantages **not yet planned** in Liberado's roadmap or ARCHITECTURE.md. These are opportunities for Liberado to close the competitive distance.

Source material derived from Hermes GitHub, DeepWiki architecture documentation, and direct exploration of Hermes subsystems (skills, memory, cron, subagents, execution environments, compression, etc.).

---

## Executive Summary — Key High-Leverage Gaps

Hermes has four categories of capability Liberado currently lacks or has only superficially:

1. **Closed Self-Improvement Loop** (Skills + Memory + Honcho)
2. **Persistent Background Automation** (Cron + Scheduled Delivery)
3. **Multi-Environment Execution Abstraction** (Docker/SSH/Modal/Daytona hibernation)
4. **Subagent Delegation + Parallel Workflows**

Each is described below with concrete Hermes implementation notes and recommended Liberado counter-features.

---

## 1. Closed Self-Improvement Loop (Skills System)

### What Hermes Has
- Agent can **create, register, and iteratively improve its own Python skills** at runtime via the `skill_manage` tool.
- Skills live in `~/.hermes/skills/`, are dynamically discovered/loaded on every turn.
- **Skills Hub** (agentskills.io) provides a public marketplace for sharing/distributing skills.
- Skills receive **automatic versioning + usage telemetry**; the agent can propose improvements to its own skills after repeated use.
- **Lifecycle hooks**: `on_load`, `on_error`, `on_success` allow self-debugging and evolution.

### Liberado Status
- Liberado has static MCP tool bindings declared in `topology.toml`.
- No runtime skill creation, no Python-skill registry, no marketplace, no self-modification.

### Recommended Action
**Rust-centric alternative that preserves Liberado's architecture:**

- The agent emits a `ProposeMcp { spec, rationale }` tool call instead of directly writing code.
- A dedicated **coding agent** (spawned via the subagent pattern or the existing orchestrator) receives the proposal, implements the MCP in Rust (or Rust+WASM), builds it, and registers the new transport entry in `topology.toml`.
- The main orchestrator performs a hot-reload or daemon restart to activate the new MCP.
- All generated MCPs carry provenance linking back to the originating dispatch/session/goal.

This keeps the entire system inside the Rust/MCP paradigm, avoids introducing a Python skills runtime, and re-uses the dispatcher, executor, and provenance machinery already present.

### Competitive Impact
High — turns the agent from a static tool user into a self-extending system. This is Hermes' signature differentiator.

---

## 2. Persistent Background Automation (Cron / Scheduled Tasks)

### What Hermes Has
- Built-in `croniter`-based scheduler.
- Natural-language task definitions (`"every night at 2am, summarize the vault and post to Telegram"`).
- Delivery targets any configured platform (CLI, Telegram, Discord, Email, etc.).
- Tasks survive restarts; results are delivered even if the user is offline.
- Integrated with the same permission/zone model.

### Liberado Status
- No scheduler. Daemon is reactive (file watcher) only.
- No persistent task queue or timed delivery.

### Recommended Action
- Add **cron** section to `tuning.toml` or a new `schedule.toml`.
- Implement a `CronRunner` inside the daemon that parses schedules and dispatches to the orchestrator with a `ScheduledGoal` envelope.
- Support delivery back through any channel (chat sessions, webhooks, messaging bridges).

### Competitive Impact
High — enables reliable unattended operation (nightly reports, backups, audits) which Liberado currently cannot do.

---

## 3. Multi-Backend Execution Environments + Hibernation

### What Hermes Has
- Six terminal backends: local, Docker, SSH, Singularity, **Modal**, **Daytona**.
- Modal/Daytona provide **serverless hibernation** — the agent's workspace persists but costs nothing while idle; wakes on next tool call.
- Backend selection is declarative in `config.yaml`; the abstraction layer hides `ptyprocess`/`winpty` differences.
- Cleanup hooks (`cleanup_vm`, `cleanup_browser`) ensure resource hygiene per turn.

### Liberado Status
- MCP tools run in the host environment only (stdio or HTTP).
- No containerization, no remote SSH, no hibernation backends.

### Recommended Action
- Extend the **executor** crate with an `ExecutionEnvironment` trait.
- Implement at minimum: `Local`, `Docker`, and one serverless option (Modal or a future self-hosted equivalent).
- Store environment state keyed by session or vault so long-running agents retain workspace context across reboots.

### Competitive Impact
Medium-High — dramatically lowers the cost and friction of always-on agents while adding isolation.

---

## 4. Subagent Delegation & Parallelism

### What Hermes Has
- `spawn_subagent` tool that launches an isolated `AIAgent` instance with restricted context and toolset.
- Subagents communicate via RPC or shared memory channels; results are folded back into the parent turn with near-zero token cost.
- Designed for parallel workstreams and "zero-context-cost turns" when scripted subagents do heavy lifting.

### Liberado Status
- Single-threaded executor loop per dispatch.
- No delegation primitive; orchestrator handles one goal at a time.

### Recommended Action
- Add a `delegate` MCP tool (or internal orchestrator primitive) that can spin up child executor instances.
- Define a lightweight subagent protocol (goal + capability subset + return channel).
- Wire provenance so subagent actions are attributed back to the parent dispatch.

### Competitive Impact
Medium — enables scaling complex goals across multiple focused agents without exploding the main context.

---

## Additional Notable Gaps (Lower Leverage)

- **Honcho dialectic memory** — cross-session user modeling beyond simple `MEMORY.md`.
- **Trajectory compression + datagen pipeline** — Hermes can export compressed trajectories for fine-tuning; Liberado has none.
- **Messaging gateway breadth** — Telegram/Discord/Slack/WhatsApp/Signal bridges out of the box.
- **Visual / computer-use tools** — browser automation, vision analysis, LSP integration.
- **ACP / IDE protocol server** — direct editor integration (Zed, VS Code) via the Agent Client Protocol.

These are valuable but secondary to the four core gaps above.

---

## Strategic Recommendation

Prioritize the **Closed Self-Improvement Loop** (skills) first — it is Hermes' strongest unique selling point and compounds every other feature. Follow immediately with **Cron scheduling** to unlock unattended operation. Execution environments and subagent delegation can follow as scaling/ops improvements.

Add explicit tickets or decision records for each to `ARCHITECTURE.md` and the roadmap so these competitive gaps are tracked rather than discovered later by users comparing Liberado to Hermes.

---

*Generated 2026-06-26. All gaps described are absent from Liberado's current ARCHITECTURE.md and roadmap.*