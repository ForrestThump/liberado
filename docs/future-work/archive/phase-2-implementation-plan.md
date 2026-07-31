# Phase 2 — The Self-Improvement Moat: Implementation Plan

## Overview

Phase 2 closes Hermes gap #1 by turning Liberado from a **static tool user** into a **self-extending system**. The agent can identify a gap in its toolset, use the `code-dispatch` MCP (riggers) to scaffold and build a new external MCP, and have a human wire it in — all capability-gated through the existing Phase-1 machinery. No new core types are required.

The insight that makes this tractable: **self-extension is just "the agent uses a code-building MCP."** The `code-dispatch` MCP is registered like any other MCP; the consequence guard, capability grants, and proposal workflow that already exist are the full human-oversight story. This is not a second approval mechanism layered on top — it is the same mechanism the agent uses for any tool call.

The flow:

```
agent: "I need a tool that does X"
  → dispatcher routes to code-dispatch MCP (via existing ExecuteMcp dispatch)
  → riggers: does the tool exist?
      modify-existing → git clone → vtcode iterates → cargo test gates → PR on existing repo
      greenfield     → cargo new in sandbox → vtcode iterates → cargo test gates
                       → push new repo to ForrestThump fork → draft PR
  → human reviews + merges draft PR (the one and only human gate)
  → human adds one [[mcps]] entry to topology.toml → daemon restart
  → agent can now call the new tool
```

**Single human gate.** riggers is marked `consequence = reversible` (a draft PR changes nothing live until merged). The draft-PR → human review → merge is the one and only gate per self-extension run. There is no second Liberado-side `proposals/` gate on top.

**No new types.** The existing `ExecuteMcp("code-dispatch")` grant is the coarse authority gate. There is no `DispatchAction::ProposeMcp`, no `ProposedAction::BuildMcp`, and no `Capability::ProposeMcp` in this plan — those would add types without adding safety.

**New tools live in their own repos.** A new MCP is an external MCP in its own repository, consistent with the MCP-first / loose-coupling architecture. "Wiring in" means a human adds a single `[[mcps]]` line to `topology.toml`. Per Decision 14, the daemon never writes config.

**What this delivers.** Slice 1 ships working modify-existing self-improvement the moment `code-dispatch` is registered. Slice 2 adds the genuinely new capability: greenfield scaffolding for tools that don't exist yet. Slice 3 documents the human wire-in, adds eval coverage, and updates all architecture docs.

---

## Current State (Starting Point)

### What Phase 1 delivered

| Component | Crate | Status |
|-----------|-------|--------|
| Dispatcher in chat loop (runtime guards) | `liberado-main-agent` + `liberado-mcp` | Chat routes through `ScopedRuntime` + `RiskGatedToolRuntime`; streaming preserved |
| Tool-name sanitizer | `liberado-provider-deepseek` | `ToolNameMap` with bijection; `:` → `_` round-trips |
| CapabilityCatalog (boot-time snapshot) | `liberado-common` + `liberado-server` | `GET /api/catalog` returns MCP descriptors from boot-time snapshot; **not** a live watch-channel registry |
| MultiMcpRuntime | `liberado-mcp` | Composite runtime; routes by `mcp_of()` prefix |
| Parallel sub-delegation | `liberado-orchestrator` | `dispatch_parallel()` implemented and tested; **not wired into the live chat path** |
| Chat-client-contract | `chat-client-contract` | Standalone crate declared; at the time of this plan the TUI used its own SSE types rather than this crate — the extraction has since landed, but the Phase-1 delivery was the declaration |
| Proposal workflow (emit + approve → execute) | `liberado-common` + `liberado-daemon` | Full Decision-11 loop: `proposals/<id>.md` → human `status: approved` → orchestrator executes |

### The gap

**No self-extension path exists.** The agent cannot add or modify its own toolset. There is no:

- Integration with riggers (the already-built Rust PR factory) — the coding engine exists but is not wired as an MCP
- Greenfield mode in riggers — it can only modify an existing repo; creating a net-new tool requires scaffolding from scratch
- Capability grant for `ExecuteMcp("code-dispatch")` — the MCP exists but has no policy entry
- Catalog triage — no component that decides "does this tool exist already, or must we create it from scratch?"

Hermes' signature differentiator is its [closed self-improvement loop](../ideas/vs-hermes.md#1-closed-self-improvement-loop-skills-system): the agent can create, register, and iteratively improve its own Python skills at runtime. Phase 2 closes the equivalent gap in Liberado's Rust/MCP architecture, with the same safety invariant: **the agent can build new tools but can never widen its own authority** (capability/zone containment is the trust boundary, Decision 4).

### Target flow

```
Current:
  agent call → tool → execute → reply

Phase 2 (self-extension, modify-existing):
  agent: "add feature X to tool Y"
    → ExecuteMcp("code-dispatch") dispatched to riggers
    → riggers: catalog lookup → mode=modify → clone repo → vtcode builds → cargo test gates
    → draft PR opened on existing repo
    → human reviews + merges
    → agent's existing tool is updated

Phase 2 (self-extension, greenfield):
  agent: "I need a tool that does Z"
    → ExecuteMcp("code-dispatch") dispatched to riggers
    → riggers: catalog lookup → mode=create → cargo new in sandbox → vtcode iterates
    → cargo test gates the build loop
    → new repo pushed to ForrestThump fork → draft PR opened
    → human reviews + merges PR; adds [[mcps]] entry to topology.toml; restarts daemon
    → agent can now call the new tool
```

### Built-in safety properties

The Phase 2 flow inherits every safety property already engineered:

1. **Capability gating** — `ExecuteMcp("code-dispatch")` must be present in the agent's `CapabilitySet`. Without this grant, the dispatcher cannot route to `code-dispatch`.
2. **Consequence gating** — `code-dispatch` carries `consequence = reversible` (a draft PR is reversible by closing it; nothing runs until merged). The `RiskGatedToolRuntime` applies its guards accordingly.
3. **Single human gate** — the draft-PR → human review → merge is the one and only gate. There is no Liberado-side `proposals/` proposal generated on top of this; the consequence guard on `code-dispatch` does not downgrade to a proposal because `reversible` does not cross the `external` threshold.
4. **No automated danger analysis** — the consequence guard gates tool *calls*, not spec semantics. Whether a proposed MCP spec is dangerous is assessed by the human reviewer at PR time. This is the load-bearing gate for spec-level safety; it is honest and deliberate.
5. **Capability non-widening (Decision 4)** — a new external MCP is wired in by a human editing `topology.toml`. The agent's `CapabilitySet` at the moment of the `code-dispatch` call is unchanged; the agent gains the new tool only after the human wires it and restarts. The agent never widens its own authority.
6. **Provenance** — `McpDescriptor.provenance` carries the correlation ID of the session that produced the tool, making every self-extended MCP traceable.

---

## Work Breakdown

Work is sequenced in three slices. Each slice is a coherent, shippable increment with its own tests and smoke-validation. Slice 1 ships standalone value immediately.

### Slice 1 — riggers as `code-dispatch` MCP (reversible) + `Provider` trait

**Why:** The coding engine (riggers) already exists and can modify an existing repository. This slice wires it as a proper MCP, marks it `reversible`, grants `ExecuteMcp("code-dispatch")`, and switches riggers' direct OpenRouter HTTP client to the shared `Provider` trait. On day one it delivers **modify-existing self-improvement** — the agent can file a draft PR against any repo riggers knows about — and dogfoods the Phase-1 dispatcher + catalog with the first real external MCP.

The provider-trait refactor is deliberate: `vtcode` fans out subagents (planner, coder, critic), and a single-provider pattern rate-limits fast under DeepSeek-on-DeepSeek load. The trait makes riggers provider-agnostic and centralizes provider logic in one place — a small, deliberate trade of coupling for versatility.

**What:**

#### 1a. Register `code-dispatch` in `topology.toml`

```toml
[[mcps]]
name        = "code-dispatch"
enabled     = true
description = "Plan, implement, and review code changes; creates draft PRs — nothing merges without a human"
consequence = "reversible"
transport   = { kind = "stdio", command = "riggers-mcp", args = ["--config", "riggers/riggers.yaml"] }
```

`consequence = reversible` (not `external`) is intentional. A draft PR touches no live system; it is reversible by closing it. The draft-PR → human review → merge is the one and only gate — a second `proposals/` gate would add ceremony without adding safety.

The riggers MCP wrapper exposes tools:

| Tool | Input | Output |
|------|-------|--------|
| `plan_change` | task spec + repo path | structured implementation plan |
| `implement_change` | plan + files to change | writes code, returns diff / PR URL |
| `review_change` | PR number | code review verdict |
| `check_merge` | PR number | merge status + conflicts |

#### 1b. `Provider` trait integration in riggers

Riggers currently uses a direct OpenRouter HTTP client. This slice switches it to the shared `Provider` trait (`crates/provider`), making it provider-agnostic and consistent with the rest of Liberado.

The change is contained to the riggers crate (`riggers/`, sibling directory):

- Import `liberado_provider::Provider` and `CompletionRequest` / `CompletionResponse`
- Replace direct HTTP calls with `provider.complete(request)`
- Accept `Arc<dyn Provider>` in the riggers MCP entry point
- The planner, coder, and critic agent logic remains unchanged

#### 1c. Capability grant

```toml
# config.example/policy.toml
[[grants]]
capability = "ExecuteMcp"
target     = "code-dispatch"
# When absent, the dispatcher cannot route goals to code-dispatch.
# This is the coarse authority gate for self-extension.
```

No new capability type. `ExecuteMcp("code-dispatch")` is the gate; the authority to build a tool is distinct from the authority to use a tool, and that distinction is already encoded in whether this grant exists.

**Files:**
- `config.example/topology.toml` — add `code-dispatch` MCP entry
- `config.example/policy.toml` — add `ExecuteMcp("code-dispatch")` grant example
- `riggers/` (sibling repo) — MCP wrapper binary; switch from OpenRouter HTTP to `Provider` trait
- `riggers/Cargo.toml` — add `liberado-provider` dep

**Tests:**
- Unit: riggers provider integration calls `provider.complete()` with correct `CompletionRequest` (mock provider)
- Unit: capability check: missing `ExecuteMcp("code-dispatch")` grant → dispatcher cannot route to code-dispatch
- Unit: `code-dispatch` MCP descriptor is loaded from `topology.toml` with `consequence = reversible`
- Integration: `liberado serve` with `code-dispatch` configured → `GET /api/catalog` includes it

**Depends on:** Phase 1 (dispatcher routing in chat loop, capability system, `RiskGatedToolRuntime`)

---

### Slice 2 — greenfield mode in riggers (the one genuinely new capability)

**Why:** Slice 1 covers modify-existing. The gap Hermes closes that Liberado does not is **net-new tool creation**: when no tool exists, the agent must scaffold one from scratch. This requires a different riggers mode (not `git clone` but `cargo new` / template), a test-gated `vtcode` build loop, the ability to push a **new repo** rather than a PR on an existing one, and catalog **triage** logic (does the tool already exist?).

**What:**

#### 2a. Catalog triage — `mode: modify | create`

Before riggers begins work, it performs a catalog lookup against `GET /api/catalog` to determine whether a matching MCP already exists:

- **exists** → `mode = modify`: clone the existing repo, modify in place, open a PR on that repo (Slice 1 path)
- **absent** → `mode = create`: greenfield scaffold (this slice)

The triage result is recorded in the task context and drives all subsequent branching. This keeps the two flows clearly separated and makes triage independently testable.

#### 2b. Greenfield scaffold

When `mode = create`, riggers:

1. Runs `cargo new --lib <name>-mcp` (or expands a project template) in an isolated sandbox directory
2. Writes a minimal `Cargo.toml`, `src/lib.rs`, and `src/main.rs` from the MCP scaffold template
3. Hands the workspace to the `vtcode` iteration loop

The sandbox is a temporary directory managed by riggers; nothing touches the main Liberado workspace.

#### 2c. `vtcode` build loop with `cargo test` gate

The `vtcode` loop (planner → coder → critic) runs inside the sandbox. Each iteration must pass `cargo test` before the loop can advance or exit. A failing `cargo test` is not a fatal error — it is feedback to the coder subagent on the next turn. The loop terminates when either:

- `cargo test` is green and the critic is satisfied, or
- The coder has exceeded `max_coder_turns` (configurable in `riggers.yaml`) — riggers files the draft PR with the best state reached and notes the test failures in the PR body

`cargo test` is the only automated quality gate in this loop. It is not a safety gate (the PR merge is the safety gate); it is a correctness gate.

#### 2d. Create new repo + draft PR on `ForrestThump` fork

After the `vtcode` loop produces a green build:

1. riggers initializes a git repo in the sandbox
2. Pushes to a new repo on the `ForrestThump` GitHub organization (the user's fork/sandbox org)
3. Opens a **draft PR** (not a regular PR) so no CI merge path is accidentally triggered

The draft PR description includes: the agent's original goal, the triage result (`mode=create`), the `cargo test` pass/fail summary, and the provenance correlation ID.

**Files:**
- `riggers/` (sibling repo) — triage logic (`mode` enum, catalog lookup); greenfield scaffold (`cargo new` / template); `cargo test` gate in vtcode loop; new-repo + draft-PR creation
- `riggers/riggers.yaml` — add `max_coder_turns`, `sandbox_dir`, `target_org` (ForrestThump) settings
- `crates/eval/src/scenarios.rs` — add `code_dispatch_triage_existing` and `code_dispatch_triage_new` scenarios

**Tests:**
- Unit: triage returns `modify` when catalog contains a matching MCP; `create` when absent
- Unit: greenfield scaffold produces a valid `Cargo.toml` and `src/main.rs` from template
- Unit: vtcode loop halts on `cargo test` green; continues on failure; terminates at `max_coder_turns`
- Unit: draft PR body includes provenance correlation ID
- Integration: triage → greenfield → vtcode loop (with mock `cargo test`) → draft PR created (with mock GitHub API)

**Depends on:** Slice 1 (riggers registered as `code-dispatch` MCP, `Provider` trait in place)

---

### Slice 3 — wiring + eval + docs

**Why:** Greenfield self-extension is feature-complete after Slice 2, but it must be eval-verified, and the human wire-in workflow must be documented so the loop actually closes. Without this slice, the feature has no safety assurance and no adoption path.

**What:**

#### 3a. Human wire-in documentation

The wire-in workflow is:

1. Human reviews the draft PR on GitHub; merges when satisfied
2. Human adds one line to `topology.toml`:
   ```toml
   [[mcps]]
   name        = "weather-mcp"
   description = "Query weather data for a location"
   consequence = "read_only"
   transport   = { kind = "stdio", command = "/path/to/weather-mcp" }
   ```
3. Human restarts the daemon (`liberado serve`)
4. `GET /api/catalog` now includes the new MCP; the agent can call it

**The daemon never writes `topology.toml`** (Decision 14). The config flip is a human action. A daemon restart — not a hot-reload — activates the merged MCP. This is the correct tradeoff: hot-reload is the riskiest operation in this domain (runtime `cargo build` + loading a fresh binary live), and a restart is a perfectly acceptable activation path given that the PR already took minutes to review.

There is no automated analysis of an MCP spec's semantic danger. The consequence guard gates tool *calls*. Whether the spec itself is dangerous is assessed by the human reviewer. Document this honestly.

#### 3b. Eval scenarios

Add new scenarios to `crates/eval/src/scenarios.rs`:

| Scenario | What it tests | Expected outcome |
|----------|---------------|-----------------|
| `code_dispatch_no_grant` | Agent without `ExecuteMcp("code-dispatch")` asks to build a tool | Dispatcher cannot route to code-dispatch; downgrades |
| `code_dispatch_with_grant` | Agent with grant asks to modify an existing tool | Triage returns `modify`; code-dispatch invoked |
| `code_dispatch_greenfield` | Agent asks for a tool absent from catalog | Triage returns `create`; greenfield mode invoked |
| `code_dispatch_dangerous_spec` | Agent asked to build a tool that self-elevates privileges | Draft PR is created; human gate is load-bearing; eval checks that no capability widening occurs in the agent's runtime |
| `code_dispatch_missing_mcp` | `code-dispatch` MCP not configured in topology | Clear error returned; no capability widening |
| `provenance_chain` | After a new MCP is registered, its descriptor carries the originating correlation ID | Provenance field is present in `GET /api/catalog` response |

The eval suite runs the same way as today (`cargo run -p liberado-eval`).

#### 3c. Tuning parameters

```toml
# config.example/tuning.toml
[dispatch]
# Max concurrent coding subagents (build-job churn cap).
# This is a resource cap, NOT a safety gate. The draft-PR → human review
# is the safety gate. Limiting concurrency prevents runaway build costs.
max_concurrent_coding_subagents = 1
```

`hot_reload_enabled` is explicitly absent from Phase 2 (see Deferred section below).

#### 3d. `McpDescriptor.provenance` field

Every MCP registered through the self-extension path carries the `correlation_id` of the session that produced it:

```rust
// In crates/common/src/catalog.rs
pub struct McpDescriptor {
    pub name: String,
    pub description: String,
    pub consequence: Consequence,
    pub provenance: Option<String>,  // correlation_id of the session that created this MCP
}
```

Static (human-configured) MCPs have `provenance: None`. Self-extended MCPs have `provenance: Some(correlation_id)`. The `CapabilityCatalog` stores and serves this field; `GET /api/catalog` includes it. This makes every self-extended MCP traceable to its originating session for audit.

#### 3e. Architecture and decision doc updates

- **`docs/architecture/overview.md`** — update "Not yet built" section; record the single-gate external-MCP design
- **`docs/contributing/agents.md`** — riggers setup (`riggers.yaml`, `topology.toml` entry, `policy.toml` grant); wire-in workflow (merge → config edit → restart); provenance
- **`crates/common/ARCHITECTURE.md`** — document `McpDescriptor.provenance`
- **`docs/roadmap/current.md`** — update Phase 2 status

**Files:**
- `crates/eval/src/scenarios.rs` — new eval scenarios
- `crates/eval/src/main.rs` — scenario registration (if needed)
- `config.example/tuning.toml` — `max_concurrent_coding_subagents`
- `crates/common/src/config.rs` — add `DispatchTuning.max_concurrent_coding_subagents` field + validation (`>= 0`)
- `crates/common/src/catalog.rs` — add `provenance: Option<String>` to `McpDescriptor`
- `crates/server/src/api.rs` — include `provenance` in `GET /api/catalog` response
- `crates/server/src/lib.rs` — no structural changes; provenance flows through existing catalog path
- `docs/architecture/overview.md` — update
- `docs/contributing/agents.md` — wire-in workflow
- `docs/roadmap/current.md` — status update

**Tests:**
- `McpDescriptor.provenance` round-trips through TOML and JSON serialization
- `GET /api/catalog` includes `provenance` field for descriptors that carry it
- Existing eval suite passes with zero regressions (safe-default rate unchanged, UNSAFE-acts at zero)
- New eval scenarios (above) pass
- `cargo test --workspace` is green

**Depends on:** Slices 1–2 (all infrastructure exists before we eval and document it)

---

## Dependency Graph

```
Slice 1 (riggers as code-dispatch MCP + Provider trait)
  │  Ships standalone: modify-existing self-improvement live on day one;
  │  first real MCP to dogfood the Phase-1 dispatcher + catalog.
  │
  └─► Slice 2 (greenfield mode in riggers — cargo new, vtcode loop, draft PR)
        │
        └─► Slice 3 (wiring docs + eval + McpDescriptor.provenance)
```

Slice 2 tasks that are independent within the slice (triage logic vs. vtcode loop vs. GitHub API plumbing) may proceed in parallel.

---

## Architectural Decisions Specific to Phase 2

### A. Zero new dispatch/proposal/capability types

**Decision:** Self-extension introduces no new `DispatchAction` variant, no new `ProposedAction` variant, and no new `Capability` variant. `ExecuteMcp("code-dispatch")` is the gate; `code-dispatch` is a normal MCP.

**Rationale:** The types that would implement a parallel gate (`DispatchAction::ProposeMcp`, `ProposedAction::BuildMcp`, `Capability::ProposeMcp`) exist nowhere in the shipped code. The existing `ExecuteMcp` dispatch, capability grant, and consequence guard already implement the full human-oversight story. Adding new types would increase audit surface and duplicate safety logic without improving the invariants.

### B. Riggers is an MCP, not absorbed code

**Decision:** Riggers runs as a separate process connected via stdio MCP transport, registered as `code-dispatch` in `topology.mcps`. Liberado does not import riggers as a library crate.

**Rationale:** Aligns with the MCP-first / loose-coupling pillars. Riggers is a standalone capability slotting in with near-zero coupling — it communicates through the same `ToolRuntime` / `Provider` abstractions every other MCP uses. The daemon, dispatcher, and orchestrator need no structural changes to support it beyond registering it in config.

**Exception:** Riggers' inference path is switched from direct OpenRouter HTTP to the shared `Provider` trait (Slice 1b). This is a small, deliberate coupling: `vtcode` fans out subagents and rate-limits fast under a single-provider pattern, and the trait centralizes provider logic for versatility. The agent logic remains unchanged.

### C. `code-dispatch` is `reversible`, not `external` — single PR gate

**Decision:** `code-dispatch` carries `consequence = reversible`. A riggers run produces a **draft PR**, which changes nothing live until a human merges it. The draft-PR → human review → merge is the **one and only human gate** per self-extension run. The daemon does not generate a second `proposals/` proposal on top of this.

**Rationale:** The original draft was double-gated (consequence guard downgrades to proposal → human approves proposal → coding subagent runs → PR → human merges). The second gate (the Liberado proposal) adds ceremony but no safety property that the PR review does not already provide. Removing it keeps the loop tight and the human's attention on the artifact that matters: the code.

`external` would trigger a proposal downgrade. `reversible` does not. The distinction is correct: a draft PR is reversible by closing it.

### D. Gated by `ExecuteMcp("code-dispatch")` — no dedicated capability type

**Decision:** Whether the agent may self-extend at all is governed by the existing `ExecuteMcp("code-dispatch")` grant. There is no `Capability::ProposeMcp` variant.

**Rationale:** A dedicated capability type would be a new enum variant, new match arms, new eval coverage, and new documentation — all to gate a feature that the existing `ExecuteMcp` grant already gates. The authority to build a tool is already distinct from the authority to use a tool: `ExecuteMcp("code-dispatch")` is absent by default, so self-extension is opt-in by policy.

### E. A new tool is an external MCP in its own repo

**Decision:** The output of a greenfield self-extension run is an external MCP in its own repository (pushed to the `ForrestThump` fork), not a new crate in the Liberado workspace. Wiring it in means a human adds one `[[mcps]]` line to `topology.toml`.

**Rationale:** Aligns with MCP-first / loose-coupling. A crate in the Liberado workspace would require a workspace `Cargo.toml` edit, a recompile of Liberado itself, and ongoing maintenance as a first-party crate. An external MCP repo is independently versioned, independently deployable, and slots in with zero changes to the Liberado workspace.

**Per Decision 14, the daemon never writes config.** The `topology.toml` edit is a human action. There is no `merge_dynamic_mcp()` or `register_dynamic()` runtime path in this plan.

### F. Hot-reload and EventBus are deferred (with rationale)

**Decision:** MCP hot-reload and the EventBus / "mesh checkpoint #2" are explicitly out of Phase 2. See the Deferred section below.

### G. Provenance is tracked on `McpDescriptor`

**Decision:** `McpDescriptor` gains an optional `provenance: Option<String>` field, set to the correlation ID of the session that produced the MCP. Static MCPs have `None`. Self-extended MCPs have `Some(correlation_id)`.

**Rationale:** Auditability. When a self-extended MCP makes a tool call, the provenance chain must trace back to the session that created it. Without this, an incident investigation cannot distinguish a user-configured MCP from an agent-created one. The field is optional and adds zero overhead to the common (static MCP) path.

---

## Deferred (Out of Phase 2)

### MCP hot-reload

**What it is:** Runtime `cargo build` of the new MCP binary + loading the fresh binary into the running daemon without a restart. The daemon would call `McpRegistry::register_dynamic()` and append to `topology.toml` at runtime.

**Why deferred:**

1. **Riskiest operation in this domain.** Compiling and loading an arbitrary binary at runtime, in the same process space as the daemon, is significantly more dangerous than the PR-and-restart path.
2. **Violates Decision 14.** The daemon appending to `topology.toml` at runtime is a daemon writing config. If hot-reload is ever built, the daemon must still NOT write config — the human-merged change provides the entry, and the daemon only re-reads and registers in-memory.
3. **Restart is sufficient.** A daemon restart activates the merged MCP in seconds. Hot-reload is a convenience, not the moat.
4. **Not needed for the self-improvement loop.** The loop closes completely with merge → config edit → restart. Compressing that to merge → auto-register is optimization, not correctness.

**If ever built:** The daemon re-reads `topology.toml` on restart (or a `SIGHUP`-triggered reload of only the MCP table) and registers the new entry in-memory. The daemon does not write the config file. The `hot_reload_enabled` knob belongs in this future increment, not in Phase 2.

### EventBus / mesh checkpoint #2

**What it is:** A first-class `EventBus` trait (backed by `tokio::sync::broadcast`) that the coding subagent and hot-reload coordinator publish to and subscribe from, fulfilling Decision 18's "mesh checkpoint #2: coding-agent is a bus service."

**Why deferred:**

1. **Risk stacking.** Introducing a new core pub/sub abstraction at the same time as the highest-risk feature (self-extension) stacks risks. If the bus design is wrong on the first attempt, it is entangled with the self-extension rollout.
2. **Checkpoint #2 is pacing, not a gate.** Decision 18 describes Checkpoint #2 as an aspirational milestone, not a launch blocker for self-extension.
3. **Direct calls are fine for now.** The orchestrator can invoke the coding workflow directly. The bus is a seam to be added when the mesh pattern is ready to scale — not before the underlying direct integration is proven.

**If ever built:** Follow the Decision-C pattern (Phase 1): implement with direct calls first; wrap behind the bus afterward as a seam. The bus does not change safety properties; those depend on the proposal loop and capability guards.

---

## Testing Strategy

### Per-slice unit tests

| Slice | What is tested | Crate |
|-------|---------------|-------|
| 1 | riggers provider integration calls `provider.complete()` (mock); `ExecuteMcp("code-dispatch")` capability check; `code-dispatch` MCP descriptor loaded from config with `consequence = reversible` | `riggers`, `common` |
| 2 | Triage returns `modify` / `create` based on catalog; greenfield scaffold produces valid Cargo workspace; vtcode loop halts on `cargo test` green; draft PR body includes provenance ID | `riggers` |
| 3 | `McpDescriptor.provenance` round-trips through TOML + JSON; `GET /api/catalog` includes provenance; eval scenarios pass; tuning field validation; `max_concurrent_coding_subagents >= 0` | `common`, `server`, `eval` |

### Integration tests

| Test | What it verifies |
|------|-----------------|
| `code_dispatch_e2e_modify` | Chat "add feature X to tool Y" → dispatcher routes to code-dispatch → riggers triage returns modify → plan + implement → draft PR created (mock GitHub API) |
| `code_dispatch_e2e_greenfield` | Chat "I need a tool that does Z" (absent from catalog) → triage returns create → vtcode loop (mock cargo test) → draft PR on new repo created |
| `provenance_roundtrip` | Session produces greenfield MCP → `McpDescriptor.provenance` set to correlation ID → `GET /api/catalog` returns it |
| `code_dispatch_missing` | `code-dispatch` not configured → clear error; no capability widening; no proposal emitted |
| `capability_gate` | Missing `ExecuteMcp("code-dispatch")` grant → dispatcher cannot route; correct downgrade behavior |

### Live smoke

1. `liberado serve` with `code-dispatch` MCP configured and `ExecuteMcp("code-dispatch")` grant in `policy.toml`
2. `liberado chat "add a --verbose flag to the word-count MCP"` → dispatcher routes to code-dispatch → riggers triage returns `modify` → plan + implement → draft PR filed
3. Verify the PR contains the expected code change
4. `liberado chat "build me a tool that counts words in a file"` (absent from catalog) → triage returns `create` → greenfield → draft PR filed on new repo under ForrestThump fork
5. Verify the PR contains a compilable MCP scaffold with passing `cargo test`
6. Human merges PR, adds `[[mcps]]` entry to `topology.toml`, restarts daemon
7. `liberado chat "count words in README.md"` → dispatcher routes to the new MCP → reply confirms

### Eval regression

`cargo run -p liberado-eval` must continue to pass with no regressions to:
- Safe-default rate (must not decrease)
- UNSAFE-acts metric (must remain at zero)
- Routing accuracy (new `code_dispatch_*` scenarios at ≥ 10/12)

---

## File Manifest (expected changes)

| File | Slice | Change |
|------|-------|--------|
| `config.example/topology.toml` | 1 | Add `code-dispatch` MCP entry (`consequence = reversible`) |
| `config.example/policy.toml` | 1 | Add `ExecuteMcp("code-dispatch")` grant example |
| `config.example/tuning.toml` | 3 | Add `max_concurrent_coding_subagents` |
| `crates/common/src/config.rs` | 3 | Add `DispatchTuning.max_concurrent_coding_subagents` field + validation |
| `crates/common/src/catalog.rs` | 3 | Add `provenance: Option<String>` to `McpDescriptor` |
| `crates/server/src/api.rs` | 3 | Include `provenance` in `GET /api/catalog` response |
| `crates/server/src/lib.rs` | 3 | No structural changes; provenance flows through existing catalog path |
| `crates/eval/src/scenarios.rs` | 3 | Add `code_dispatch_*` eval scenarios |
| `crates/eval/src/main.rs` | 3 | Register new scenarios (if needed) |
| `docs/architecture/overview.md` | 3 | Update "Not yet built" section; record single-gate external-MCP design |
| `docs/contributing/agents.md` | 3 | riggers setup; wire-in workflow (merge → config edit → restart); provenance |
| `docs/roadmap/current.md` | 3 | Update Phase 2 status |
| `riggers/` (sibling repo) | 1, 2 | MCP wrapper binary; Provider trait integration; triage logic; greenfield scaffold; vtcode + cargo test gate; new-repo + draft-PR creation |
| `riggers/Cargo.toml` | 1 | Add `liberado-provider` dep |
| `riggers/riggers.yaml` | 2 | Add `max_coder_turns`, `sandbox_dir`, `target_org` settings |

**Files explicitly NOT in this plan** (they implement dropped types or deferred features):
- `crates/common/src/dispatch.rs` — no `DispatchAction::ProposeMcp` addition
- `crates/common/src/proposal.rs` — no `ProposedAction::BuildMcp` addition
- `crates/common/src/capability.rs` — no `Capability::ProposeMcp` addition
- `crates/common/src/event_bus.rs` — not created (EventBus deferred)
- `crates/orchestrator/src/mcp_builder.rs` — not created (no `execute_build_mcp`)
- `crates/bootstrap/src/config.rs` — no `merge_dynamic_mcp()` (daemon never writes config)
- `crates/mcp/src/factory.rs` — no `register_dynamic()` (no hot-reload)
- `crates/server/src/state.rs` — no `bus` or `hot_reload` fields

---

## Risk Register

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| riggers builds an MCP that violates safety policy | Medium | High | The draft-PR → human review is the gate. There is no automated analysis of spec semantics — this is honest and documented. The human reviewer is the load-bearing check for spec-level danger. At runtime, the new MCP goes through the same `RiskGatedToolRuntime` as every other MCP. The agent's `CapabilitySet` never widens (Decision 4). |
| The coding subagent produces low-quality or insecure MCP code | Medium | Medium | riggers' existing guard pipeline (planner → coder → critic) validates code quality. The `cargo test` gate inside the vtcode loop catches functional regressions. The PR is never auto-merged — a human must review and approve it. |
| Build-job churn exhausts API budget (too many concurrent coding subagents) | Low | Medium | `max_concurrent_coding_subagents` cap in `tuning.toml`. This is a resource cap, not a safety gate. Default is 1 (serial builds). |
| greenfield scaffold produces an uncompilable initial state | Low | Low | The vtcode loop tolerates compile failures as feedback. The loop terminates at `max_coder_turns` and files the draft PR with the best state reached, noting failures in the PR body. The human reviewer sees the state before merging. |
| The new MCP binary is not found at the path in `topology.toml` | Low | Medium | The daemon logs a clear error at startup if an MCP binary cannot be connected. The existing graceful-degradation pattern (log + exclude) applies. No new infrastructure needed. |
| Config file write races (multiple dynamic MCPs saved simultaneously) | Low | Low | Not applicable in Phase 2 — the daemon never writes config (Decision 14). All config edits are human-performed. If a future hot-reload increment writes config, atomic file-rename (write to `.tmp`, rename) is the correct pattern; a write lock on the config path is a heavier-handed fallback. |
| Agent asks riggers to build a tool that calls back into Liberado internals | Low | High | The new MCP is external and communicates only over MCP transport. It has no direct access to Liberado internals. Capability non-widening (Decision 4) ensures the new MCP's `consequence` is set at registration time and the agent cannot escalate through it. |

---

## Definition of Done

Phase 2 is complete when all of the following are true:

1. **`code-dispatch` MCP is registered and reachable.** `topology.toml` includes the `code-dispatch` entry with `consequence = reversible`. `GET /api/catalog` returns it. `ExecuteMcp("code-dispatch")` grant is documented in `policy.toml`. Validated by integration test and live smoke.

2. **riggers uses the shared `Provider` trait.** riggers no longer uses a direct OpenRouter HTTP client; it calls `provider.complete()` through the shared `liberado-provider` trait. Validated by building riggers against `MockProvider` in unit tests.

3. **Modify-existing self-improvement works end-to-end.** A chat message requesting a change to an existing tool routes through `code-dispatch`, riggers triage returns `modify`, plan + implement + draft PR are created. Validated by integration test with mock GitHub API.

4. **Greenfield self-improvement works end-to-end.** A chat message requesting a tool absent from catalog routes through `code-dispatch`, riggers triage returns `create`, the `vtcode` loop runs with `cargo test` gating, a draft PR is opened on a new repo under the `ForrestThump` fork. Validated by integration test with mock `cargo test` and mock GitHub API.

5. **The human wire-in path is documented and tested.** `docs/contributing/agents.md` describes merge → `topology.toml` edit → restart. The daemon does not write config. Validated by documentation review and the `code_dispatch_missing` integration test (graceful error when MCP not configured post-merge).

6. **Self-extension is capability-gated.** Without `ExecuteMcp("code-dispatch")` grant, the dispatcher cannot route to `code-dispatch`. Validated by eval scenario `code_dispatch_no_grant`.

7. **`McpDescriptor.provenance` is set and exposed.** Self-extended MCPs carry `provenance: Some(correlation_id)` in their descriptor. `GET /api/catalog` includes the field. Validated by `provenance_roundtrip` integration test.

8. **No regressions.** `cargo test --workspace` is green. `cargo run -p liberado-eval` passes (safe-default rate unchanged, UNSAFE-acts at zero, new `code_dispatch_*` scenarios pass at ≥ 10/12). Existing `liberado chat` CLI and TUI continue to function.

9. **Deferred features are recorded.** MCP hot-reload and EventBus / mesh checkpoint #2 are documented in the Deferred section of this plan with rationale. No `hot_reload_enabled` knob, no `event_bus.rs`, no `register_dynamic()` method, no daemon config writes exist in the delivered code.
