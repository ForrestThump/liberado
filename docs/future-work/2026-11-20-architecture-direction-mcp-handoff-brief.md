---
kind: plan
status: draft
authority: advisory
domain: product
canonical_for: architecture-direction-mcp-handoff-brief
open_items: true
---

> **Working proposal, not current authority.** Current product direction is [`docs/direction.md`](../direction.md). This brief does not replace the backlog, accepted ADRs, or the spec. Agents must not treat this file as the implementation queue.

# Liberado: Architecture Direction and MCP Integration — Handoff Brief

**Purpose:** Transfer the current design discussion to another model so it can help refine the architecture, scope, implementation plan, and technical decisions.

**Status:** Working proposal, not a finalized specification. This document consolidates the conversation and adds explicit caveats where claims need verification.

---

## 1. Executive summary

The central proposal is to **stop treating Liberado as a complete AI orchestration product that must compete with heavily funded platforms** and instead make it a **Rust-native personal automation and execution layer that AI assistants can access through MCP**.

Under this model:

- Commercial assistants such as Claude, ChatGPT, Grok, or other compatible clients provide the reasoning when the user interacts with them.
- Liberado provides durable state, personal data, scheduling, workflow execution, permissions, approvals, audit history, and integrations.
- MCP is an interoperability boundary, not the internal architecture of Liberado.
- Liberado's core daemon does not need to perform model inference for ordinary interactive use. The assistant performs inference and calls Liberado's tools.
- Liberado should remain useful without an AI model running: deterministic schedules, reminders, and workflows continue independently.
- The system should be provider-agnostic. Switching assistants should not require migrating personal data or rebuilding workflows.

The practical recommendation is to **keep the Rust daemon and valuable infrastructure, sharply reduce product scope, and use existing AI clients and MCP servers instead of rebuilding their functionality**.

A useful shorthand:

> Liberado is the part of the user's AI ecosystem that they own; model intelligence is rented from whichever provider offers the best product.

This is not a recommendation to abandon Liberado. It is a recommendation to change what Liberado is trying to be.

## 2. Original motivation and constraints

The original motivation for Liberado included:

1. Avoiding lock-in to a single AI vendor.
2. Keeping personal data, workflows, and automation state under the user's control.
3. Having a system that can work with multiple AI providers and eventually locally hosted models.
4. Avoiding the resource overhead and implementation preferences of orchestration projects built primarily in Python or TypeScript.
5. Building a native Rust system with low baseline resource usage, predictable memory behavior, straightforward deployment, concurrency, and strong type safety.
6. Avoiding unnecessary SaaS dependencies, subscriptions, telemetry, and vendor-controlled persistence.
7. Creating a personal life/automation system rather than another disposable chat interface.

The user has already made meaningful progress. Existing architectural ideas include a Rust daemon, file watching, cron/scheduled work, webhooks, an LLM/MCP integration layer, a dispatcher that can execute, clarify, delegate to a subagent, or request approval, and a proposal/approval path for consequential actions. The project has emphasized Rust-first design and provider-agnostic interfaces.

The user's practical constraint is limited discretionary development time alongside a full-time engineering job and family responsibilities. The project must justify its ongoing maintenance burden.

## 3. Why the original scope is being reconsidered

The user is concerned that building and polishing a full orchestration product is a poor solo-development investment:

- Large commercial organizations can invest heavily in UX, integrations, reliability, testing, documentation, and rapid adaptation to new model features.
- Major open-source projects can benefit from many contributors.
- The agentic AI ecosystem changes quickly; features can become commoditized before a solo developer finishes implementing them.
- Dogfooding and UX refinement can consume substantial time without creating durable differentiation.
- API inference can be expensive for multi-step agentic workflows.
- Paying for another inference subscription is hard to justify when the user already has commercial AI subscriptions.

The revised strategy is to own the components that create durable personal value and consume commodity capabilities from existing projects and vendors.

## 4. Revised product thesis

### 4.1 What Liberado should own

Liberado should focus on durable, provider-independent capabilities:

- **Persistent personal state:** tasks, reminders, workflow definitions, durable records, and user-owned data.
- **Scheduling:** cron-like scheduling, recurring actions, and event-triggered workflows.
- **Execution engine:** deterministic execution of approved actions and workflows.
- **Permissions and approvals:** explicit read/write scopes, approval gates, and policy enforcement independent of the model.
- **Task lifecycle:** creation, status, results, retries, history, and audit trail.
- **Provider-independent workflow definitions:** workflow state should not be coupled to a specific model's conversation format or API.
- **Integration boundary:** interfaces to local applications, files, Home Assistant, GitHub, and other services as needed.
- **A small local CLI / daemon interface:** a dependable native service, not necessarily a polished consumer UI.

### 4.2 What Liberado should not try to own initially

Avoid rebuilding or competing on:

- A general-purpose chat interface.
- A complete general agent runtime.
- Broad model-provider coverage for its own sake.
- A large visual workflow editor.
- Every possible productivity integration.
- A proprietary alternative to MCP unless a concrete limitation requires one.
- An elaborate UI before core personal workflows prove useful.
- Reimplementation of mature MCP servers without a compelling reason.

### 4.3 Rust as foundation, not dogma

Keep the core daemon and critical infrastructure Rust-native. However, do not require every integration or external component to be Rust. A Rust daemon can communicate with a third-party Python or TypeScript MCP server over standard interfaces when that is cheaper and more reliable than reimplementing it.

The key metric is not purity of language choice; it is the utility and reliability of the complete system. Avoiding garbage collection and reducing baseline memory use are legitimate preferences, but they should not become a source of needless reimplementation work.

## 5. Target architecture

### 5.1 High-level model

```text
       AI assistants / clients
   Claude | ChatGPT | Grok | other clients
                 |
          MCP / local adapters
                 |
       Liberado MCP interface(s)
                 |
         Internal capability API
                 |
          Liberado Rust daemon
                 |
   +-------------+-------------------+
   |             |                   |
Persistence   Scheduler          Execution engine
   |             |                   |
   +-------------+-------------------+
                 |
       Permissions / approvals
                 |
     Local apps, services, data,
       and external integrations
```

The MCP layer should be an adapter around Liberado's own typed capability interface. MCP should not become the daemon's internal API or dictate its persistence and execution model.

This separation preserves the option to support future protocols, CLI clients, direct local integrations, or new agent runtimes without rewriting the application.

### 5.2 Responsibilities by layer

| Layer | Responsibility |
|---|---|
| AI client | Reasoning, conversation, deciding which tools to call, interactive user experience |
| MCP adapter | Expose selected Liberado capabilities in a client-compatible protocol |
| Internal capability API | Typed, validated operations offered by Liberado |
| Rust daemon | Durable state, scheduling, dispatch, policy enforcement, workflow execution |
| Integrations | Interact with files, services, applications, and other MCP servers where useful |

### 5.3 Keep the daemon useful without inference

Liberado should not require an LLM to:

- Run an already-defined deterministic workflow.
- Fire a reminder.
- Execute a scheduled, pre-approved action.
- Record task state or execution history.
- Enforce permissions and approval rules.
- Retry deterministic work according to policy.

An LLM can create or modify workflows, interpret ambiguous requests, or choose among tools. The daemon should own the durable state and deterministic execution once those decisions have been made.

A critical qualification: **any background workflow that genuinely needs new reasoning still needs some inference source or a human decision**. A subscription-based chat interface does not automatically provide a durable, unattended inference service to Liberado. Zero inference cost applies when the assistant is actively used as the reasoning client and Liberado itself is not making model calls.

## 6. MCP connectivity and deployment

MCP support varies by product, plan, client, and transport. “Supports MCP” does not necessarily mean “supports arbitrary local and remote custom MCP servers in every app and subscription tier.” Compatibility must be checked against current official documentation before implementation decisions are based on it.

### 6.1 Local setup: same computer

When Claude Desktop and Liberado run on the same computer, the simplest option is a local MCP server launched by Claude using **stdio**.

```text
Claude Desktop
      |
  stdio process
      |
Liberado MCP adapter
      |
local IPC / localhost
      |
Liberado daemon
```

No Tailscale or public tunnel is required for this arrangement. The MCP adapter can communicate with the daemon using a local socket, named pipe, localhost HTTP endpoint, or another controlled IPC mechanism.

### 6.2 Remote homelab setup: desktop client, daemon elsewhere

If the AI client runs on the Windows desktop but the daemon is on a separate homelab machine, a local stdio bridge can connect over the user's Tailscale network:

```text
Claude Desktop (desktop)
      |
local stdio MCP bridge
      |
Tailscale private network
      |
Liberado MCP endpoint (homelab)
      |
Liberado daemon
```

This can avoid exposing the homelab publicly. The local adapter runs on the computer with Claude Desktop and connects to the daemon or an MCP endpoint on the homelab using its Tailscale IP or MagicDNS hostname.

Important distinction: a cloud-hosted remote connector generally originates from the provider's infrastructure, not from the user's desktop. It will not automatically be able to reach a private Tailscale address. A local desktop process that joins the tailnet can.

### 6.3 Public remote MCP setup

Some AI clients may require a publicly reachable HTTPS MCP endpoint. A tunnel can provide access without opening an inbound port on the home router:

```text
Cloud-hosted AI client
      |
public HTTPS endpoint
      |
Cloudflare Tunnel / equivalent
      |
small Liberado MCP gateway
      |
private daemon capability API
```

A named Cloudflare Tunnel is more appropriate for a persistent endpoint than an ephemeral development tunnel. Alternatives include a VPS reverse proxy or other secure ingress service. Tailscale Funnel may be an option in some scenarios, but private Tailscale connectivity and public ingress are different use cases.

**Do not expose the daemon's internal control interface directly to the public internet.** Expose a narrow MCP gateway with authentication and an explicit tool allowlist.

### 6.4 Provider compatibility checklist

For every target product, verify these separately:

1. Does the exact product and subscription plan support custom MCP servers/connectors?
2. Does it support local stdio MCP, remote HTTP MCP, or both?
3. Is custom MCP available in the desktop application, web client, mobile application, or only a developer/API environment?
4. Can it call write-capable tools, or only read/fetch tools?
5. Does it support OAuth, bearer tokens, or another authentication mechanism required by the server?
6. Does tool use count against subscription limits? Are there per-session or per-day restrictions?
7. Can the client access a local process, or does the connection originate from provider cloud infrastructure?
8. Can the assistant use the MCP integration in the interaction mode the user actually intends to use?

Products discussed include Claude Pro / Claude Desktop, ChatGPT consumer plans, ChatGPT Business/Enterprise, and SuperGrok. Previous discussion offered a rough compatibility table, but those claims should be treated as **unverified and potentially plan-dependent**, not as a reliable current compatibility matrix. Re-check official documentation for the exact plan and application before building around any provider.

## 7. One MCP server or several?

### 7.1 Recommendation

Start with **one MCP server with a small, well-organized tool catalog**. Keep the internal design modular so the tools can later be exposed through multiple MCP server instances or endpoints without changing the underlying capability implementation.

Do not split into many servers prematurely. Separate MCP servers add deployment and configuration overhead. Split when there is a concrete reason: distinct security boundary, different client audience, separate operational lifecycle, or an unwieldy tool catalog.

### 7.2 Possible logical domains

The following three domains are a possible future division, not a requirement to implement three processes immediately:

#### `liberado-personal`

Everyday personal assistant operations:

- `search_memory`
- `create_reminder`
- `list_reminders`
- `create_task`
- `update_task`
- `get_task_status`
- `search_files`

#### `liberado-automation`

Persistent workflows and scheduled execution:

- `create_workflow`
- `update_workflow`
- `list_workflows`
- `run_workflow`
- `schedule_task`
- `get_execution_history`

#### `liberado-admin`

Administrative and potentially consequential operations:

- `get_system_status`
- `inspect_permissions`
- `manage_integrations`
- `inspect_daemon_logs`
- `restart_service`
- `modify_security_policy`

Tool names and exact API shapes are illustrative. Design tools around clear user-level capabilities, not one tool for every internal function.

### 7.3 Separate processes are not required

The three logical interfaces can be separate server instances inside one Rust executable, sharing the same internal services and database. They do not necessarily need three daemon processes or separate databases.

A useful distinction:

- **One daemon:** owns state and policy.
- **One internal capability API:** owns typed operations.
- **One or more MCP adapters:** expose selected operations to different clients.

For example, a local Claude Desktop installation might receive personal, automation, and admin tools, while a remote cloud client receives only a narrowly scoped personal tool set.

### 7.4 Tool catalog size

A large tool catalog can make tool selection harder and consume context. That argues for domain grouping and selective exposure, but not arbitrary fragmentation. A cited rule of thumb from earlier discussion suggested considering a split around 50 tools; this is not an MCP protocol limit and should not be treated as a hard threshold. Actual tool count, descriptions, model behavior, and context overhead should be measured.

## 8. Security and trust boundaries

The AI model should not be trusted to enforce its own permissions. Liberado must enforce permissions in the daemon or capability layer.

Recommended controls:

- Explicit read/write scopes.
- Separate read-only and write-capable tool exposure where useful.
- Tool allowlists per client or MCP endpoint.
- Authentication for every remote connection.
- Tailscale ACLs for private network access.
- TLS for public or cross-network access.
- Approval requirements for consequential or destructive operations.
- Audit logging of tool calls, approvals, arguments, and outcomes.
- Input validation and bounded argument sizes.
- Timeouts, rate limits, and resource limits.
- Idempotency protections for actions that may be retried.
- No public exposure of the daemon's internal administrative API.
- Least privilege for the OS account running MCP adapters and the daemon.

The MCP adapter should be a constrained boundary, not a privileged tunnel into the entire machine.

## 9. Inference economics and subscription usage

### 9.1 Desired economics

The user already pays for commercial AI subscriptions and finds additional per-token API costs difficult to justify for routine interactive tasks. A key goal is to avoid paying an additional API inference bill just to use Liberado.

In the target interactive architecture:

1. The user talks to an assistant through a subscription product.
2. The assistant performs inference within that product's subscription and usage limits.
3. The assistant calls Liberado's MCP tools.
4. Liberado validates and performs the requested operation.
5. Liberado stores durable state and execution history.

Liberado itself makes no model API calls in this flow, so it incurs no inference-token charges.

### 9.2 Important limitation: subscriptions are not general-purpose inference APIs

Do not assume a subscription can be used as an unrestricted, unattended inference backend for the Liberado daemon. Consumer subscriptions usually have product-specific interfaces, usage limits, and restrictions. MCP support lets an assistant call tools; it does not necessarily give Liberado a callable endpoint for consuming that assistant's subscription quota.

This distinction affects background automation:

- Deterministic tasks can run without inference.
- Interactive tasks can use the user's subscription assistant.
- Unattended tasks that need reasoning require an agent runtime that can operate unattended, an API inference provider, a locally hosted model, or human input. The chosen option may have additional cost or operational complexity.

The architecture should make inference optional and replaceable rather than promise that every agentic workflow can run indefinitely on consumer subscription quotas.

### 9.3 Preserve a clean boundary

Model inference should be replaceable. The daemon should not depend on one provider's conversation format, proprietary memory model, or API-specific agent runtime. Provider-specific adapters belong at the edge.

## 10. Suggested initial implementation sequence

Keep the first milestone deliberately small.

### Milestone 1: establish the boundary

- Define a typed internal capability API in Rust.
- Keep that API independent of MCP and any specific LLM provider.
- Ensure it calls into the existing daemon services rather than duplicating business logic.
- Implement one MCP server using local stdio.
- Expose only a small set of genuinely useful tools.

### Milestone 2: prove interactive utility

- Configure Claude Desktop or another currently compatible client to use the local MCP server.
- Test read and write operations.
- Test approval-gated actions.
- Test that daemon state survives client restarts and provider changes.
- Use it in real personal workflows before expanding the tool catalog.

### Milestone 3: add remote connectivity only if needed

- If the daemon is on another machine, test a local stdio bridge over Tailscale.
- If a cloud-hosted client needs to call it, add a narrow remote MCP gateway.
- Use Cloudflare Tunnel or equivalent only when public HTTPS reachability is actually required.
- Keep the daemon private behind the gateway.

### Milestone 4: modularize exposure

- Add domain-based tool registration.
- Add per-client tool allowlists and permission profiles.
- Split into multiple MCP server instances only when tool discovery, security, or operations justify it.

### Milestone 5: evaluate scope based on actual use

- Freeze feature additions that lack a real personal use case.
- Avoid polishing a broad UI prematurely.
- Measure maintenance burden and practical utility.
- Keep the project dormant rather than expanding it if it stops earning its keep.

## 11. Decisions and working conclusions from the discussion

These are the current working conclusions, not immutable requirements:

1. **Re-scope, do not necessarily abandon.** Liberado should become personal AI/automation infrastructure rather than a competing general orchestration platform.
2. **Keep the Rust core.** Do not rewrite the project just to follow the dominant orchestration language ecosystem, but use existing third-party components where sensible.
3. **Own durable state and execution.** Persistence, scheduling, permissions, approvals, workflows, and task history are the strongest reasons to keep Liberado.
4. **Use MCP as an adapter layer.** Do not make MCP the daemon's internal architecture.
5. **Start local.** Claude Desktop plus local stdio MCP is the simplest first integration if supported by the current client configuration.
6. **Use Tailscale for private cross-machine access.** A local MCP bridge can access a daemon on another tailnet machine without publishing it to the internet.
7. **Use a tunnel only when a cloud-hosted client requires public reachability.** Cloudflare Tunnel is one possible approach; it is not universally required.
8. **Start with one server.** Keep internal tool domains modular and split interfaces later if useful.
9. **Enforce security in Liberado.** Never rely on the model to obey permission instructions.
10. **Avoid routine Liberado-side inference.** Interactive subscription assistants can provide reasoning while Liberado performs deterministic work.
11. **Do not assume subscription access equals an unattended inference API.** Background reasoning remains a separate problem.
12. **Protect development time.** Prefer actual personal utility and reliability over feature parity or UX competition.

## 12. Open questions for the next design iteration

A subsequent model should help resolve these without assuming answers:

### Product scope

- Which existing Liberado components are already implemented and reliable?
- Which features does the user personally use today?
- What is the minimum useful vertical slice?
- Should Liberado primarily manage tasks/reminders, arbitrary workflows, personal knowledge, or all three over time?

### Daemon and capability API

- What IPC/API should MCP adapters use to talk to the daemon: Unix domain sockets, Windows named pipes, localhost HTTP, gRPC, or another option?
- Should adapters run as subprocesses or be hosted by the daemon?
- How should capability versioning and backward compatibility work?
- What is the minimum persistence schema needed for tasks, workflow definitions, executions, approvals, and audit events?

### MCP design

- Which MCP transport(s) should be implemented first?
- What exact MCP SDK/crate is mature and appropriate for the current Rust version?
- How should tool registration, schemas, and permission metadata be represented?
- Should local stdio and remote HTTP use the same adapter implementation?
- How can the design accommodate client-specific MCP limitations without contaminating the core API?

### Client and provider compatibility

- What do the current official docs say about Claude Desktop, Claude Pro remote connectors, ChatGPT consumer plans, ChatGPT Business/Enterprise, and SuperGrok?
- Which products can connect to local stdio servers?
- Which require publicly reachable HTTPS endpoints?
- Which support write tools and user confirmation flows?
- Are there plan-specific restrictions, usage caps, or authentication requirements?
- Which target client provides the best first end-to-end development experience?

### Security

- What threat model applies to a personal homelab service?
- How should local vs remote clients be authenticated?
- How should per-client permission profiles and approval policies be stored?
- Which operations should always require human approval?
- How should credentials for integrations be stored and delegated?

### Background automation

- Which automations can be deterministic?
- Which genuinely need model reasoning?
- Should unattended reasoning use an API, a locally hosted model, or a separately managed agent client?
- How should the daemon behave when inference is unavailable, quota-limited, or too expensive?

### Maintenance and project economics

- What is the ongoing maintenance burden of one MCP adapter versus several?
- Which third-party MCP servers should be reused?
- Which components are worth implementing natively in Rust for reliability or security?
- What is the stopping rule for new features?

## 13. Guidance for further research

Provider capabilities, product plan entitlements, and MCP implementations change quickly. Any follow-up research should use current official documentation and clearly distinguish:

- consumer subscription product vs developer API;
- local stdio MCP vs remote HTTP MCP;
- assistant-side tool calling vs Liberado-side inference;
- private tailnet connectivity vs public tunnel ingress;
- interactive sessions vs unattended scheduled agents;
- read-only tools vs write-capable tools.

Avoid relying on the rough provider compatibility statements made in the earlier conversation without re-verification. The architectural direction should remain provider-agnostic even if a particular product changes or removes MCP support.

## 14. Suggested prompt for the next model

> Treat this document as the current working brief for Liberado. Help me turn it into a technically sound, minimal architecture and implementation plan. Challenge assumptions rather than merely agreeing. Prioritize a Rust-native persistent daemon, a provider-independent capability API, MCP adapters, local-first connectivity, secure remote access only when required, and practical utility over feature parity. Explicitly separate what can be done with interactive subscription assistants from what requires unattended inference. Research current official MCP/client documentation before making provider compatibility claims. Start by identifying the smallest useful vertical slice and the key architectural decisions that need to be made before coding.

---

**End of handoff brief.**
