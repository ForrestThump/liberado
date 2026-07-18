# Research: paradigms in subagent-manager-mcp (a.k.a. context-manager-mcp)

**Status:** research mining, 2026-07-17. **Not deployed and never to be deployed** — this server is
superseded by the Liberado delegator. This note exists to extract the ideas worth copying into
Liberado and to record an explicit verdict on each, so the source can be archived without loss.

Source read: `subagent-manager-mcp-master/` (repo root). Package is `context-manager-mcp`; it is a
single-crate Rust MCP server providing **durable, scope-aware project memory for agentic coding
workflows**. It answers four questions across sessions:

1. What are we trying to build right now? (current phase + task scope)
2. What decisions/events already happened? (append-only logs)
3. What phase of work is currently allowed? (phase-gate enforcement)
4. What prior context should be retrieved quickly? (checkpoints + semantic search)

---

## The paradigms, and a verdict for Liberado

Verdicts: **ADOPT** (worth building into Liberado), **REINFORCES** (Liberado already does this;
confirms the design), **SKIP** (not a fit).

### 1. Delegation gate / test-contract-before-implement — **ADOPT (highest value)**
A lower-tier worker is **structurally blocked** from reading/implementing a phase until a
supervisor-approved **test contract** (a set of *failing* tests) exists for it, or an explicit
operator exception is granted. Phase advancement clears the gate so the next phase starts fresh.

Why it matters for Liberado: the replacement priority is automation → chat → **coding**, and this is
the cleanest structural enforcement of test-first delegation I've seen — it makes "write the failing
test before you let a subagent implement" a *gate*, not a guideline. Directly applicable to
Liberado's `coding` pack and its dispatch→executor delegation. Maps onto the existing capability
model: a `SubmitTestContract` / `ApproveTestContract` capability on the supervisor, and an executor
grant that is inert until the gate opens. See [[project_channels_and_interactivity]] (interactivity
as a capability) — a delegation gate is the same shape: a capability × state, not a subtype.

### 2. Fail-closed, evidence-based auto-approval — **ADOPT / adapt**
Auto-approval is an explicitly-enabled mode (never the default). A request must carry **required
boolean metrics**, and **all** must be `true` or it is denied; the decision reuses the normal
request+approval audit trail and emits structured diagnostics explaining the pass/deny.

Why it matters: Liberado already gates far-reaching actions for human approval (observed live — the
spider scrape was held for approval because `consequence = "external"`). The missing piece is a
*principled* auto-approval path so low-risk, evidence-backed actions don't always need a human. The
"required metrics all true, else fail closed, always audited" shape is exactly the right default for
that. Adapt the metrics to Liberado's consequence/zone model (e.g. `read_only && !writes_vault &&
within_budget`).

### 3. Operator/worker tool isolation — **REINFORCES**
Operator-only controls (approve, reject, delegation management) live in the CLI and are **never**
exposed through the worker-facing MCP transport; only worker-safe tools are. Enforced by
construction (different module surfaces), not by a runtime check.

Liberado already separates the three channels (authority / information / human-input) and gates MCP
execution behind capability grants — see [[project_channels_and_interactivity]]. This is independent
confirmation that authority controls must be a *different surface*, not a flag on the worker API.

### 4. Phase-state as scope-local, plan.md-driven structured memory — **ADOPT (partial)**
Project memory is a human-readable `plan.md` split on H2 headings; each **task scope** has an
assigned worker and a list of **visible sections**; workers can only read sections their scope
exposes. Phase state (`phase_state.json`) is per-scope. Reads are checked against scope assignment →
section visibility → delegation gate, in that order.

Worth borrowing: the **section-visibility** model (a worker sees only the slice of the plan relevant
to its scope) is a concrete, low-tech way to scope context — and it composes with Liberado's vault
zones. The full flat-file scope registry is redundant with Liberado's Session model
([[project_unified_session_model]]), but the "visible_sections per scope" idea is a clean addition to
how a delegated Session is handed its context.

### 5. Inline, fire-and-forget semantic indexing with failure tracking — **ADOPT for liberado-qdrant-mcp**
On every decision/event/checkpoint append: the JSONL write **always** succeeds first; then a
best-effort `embed → index.upsert` runs that **never fails the caller**; on any error it records a
row in an `index_failures` table (with an incrementing attempt count) for later retry/rebuild. The
vector store is local SQLite — vectors as f32 BLOBs, cosine computed in Rust, excerpts truncated at
200 chars.

Why it matters: this is precisely the auto-indexing pattern `liberado-qdrant-mcp` lacks. Right now
that server only ingests on explicit `ingest_text`/`ingest_path` calls. The "durable log write is
authoritative; indexing is a best-effort side-effect with a failure ledger and a rebuild path" design
is the right way to make memory capture automatic without ever letting an embedder outage drop data.
Fold into the qdrant-mcp roadmap. (Their local-SQLite-cosine store is inferior to Qdrant for scale,
so take the *pattern*, not the storage.)

### 6. Durable append-only audit + atomic writes + advisory file locks — **REINFORCES**
All state changes produce JSONL audit records (`decisions`, `events`, `checkpoints`, `approvals`,
`diagnostics`). Writes are temp-file + `rename` (atomic), guarded by an exclusive advisory POSIX lock
on a `.lock` sibling. A startup `check_integrity()` validates JSON/JSONL parseability and cross-file
referential integrity (scope IDs in phase_state absent from task_scopes), returning warnings rather
than hard-failing.

Liberado's session store is already JSONL ([[project_unified_session_model]]). Two concrete things
worth lifting: the **startup integrity check that warns but does not fail** (matches the failure-modes
doctrine — surface corruption, let the caller decide severity), and the **advisory-lock-around-write**
helper if Liberado ever has concurrent writers to the same session file.

### 7. Thin transport / rich core (façade) — **REINFORCES**
`ContextManager` is a façade that owns storage + orchestration; all policy (access_control,
phase_flow, auto_approval, delegation_gate) lives in dedicated, independently-testable modules; the
MCP/JSON-RPC transport is minimal. This is the same "thin transport, policy in the core" split the
whole liberado-*-mcp standardization is enforcing. Independent confirmation of the house style.

---

## Verdict summary

| Paradigm | Verdict | Where it lands in Liberado |
|---|---|---|
| Delegation gate / test-contract | **ADOPT** | `coding` pack + dispatch→executor; new capability |
| Fail-closed evidence-based auto-approval | **ADOPT/adapt** | approval path over consequence/zone model |
| Inline fire-and-forget indexing + failure ledger | **ADOPT** | `liberado-qdrant-mcp` auto-index roadmap |
| Section-visibility per scope | **ADOPT (partial)** | context handed to a delegated Session |
| Operator/worker tool isolation | REINFORCES | three-channel + capability model |
| Atomic writes / advisory locks / startup integrity-warn | REINFORCES | session store hardening |
| Thin transport / rich core façade | REINFORCES | the standardization house style |

**Nothing here argues for deploying or reviving the server** — every valuable idea is a pattern to
reimplement inside Liberado's own model, not a service to run. The strongest single takeaway is the
**test-contract delegation gate**: a structural, auditable "no implementation before an approved
failing test" gate, which fits the coding-replacement priority better than anything Liberado has today.

---

## Addendum: mem0-mcp (`liberado-tool-helper-mcp`) — reviewed, not deployed

Reviewed alongside the above per operator instruction; it is **superseded by the Liberado delegator
and is not to be deployed**. It is a Rust MCP server wrapping the [mem0](https://mem0.ai) memory API
with **scope-hardcoded tools** so agents never manage `user_id`/`agent_id`/filter params. Two
isolated stores: **general** (facts, history, preferences) and **procedural** (tool-selection
guidance, proven workflows). Tools: `search_memory`/`add_memory` (general),
`search_tool_guidance`/`save_tool_guidance` (procedural), `delete_memory`.

**The one idea worth keeping — the "procedural memory" store — ADOPT (adapt).** Separating *episodic*
memory (what happened) from *procedural* memory (which tool/workflow to use for a task, saved as a
prescriptive directive for future instances) is a clean distinction. Its stated motivation —
"endlessly tweaking system prompts is annoying" — is exactly the pain a self-improving agent should
solve by *writing durable guidance to a store* instead of growing its prompt. This maps directly onto
`liberado-qdrant-mcp`: a dedicated `tool-guidance` / `procedural` collection that the delegator
consults before dispatch and writes back to after a proven run. Combined with paradigm #5 (inline
fire-and-forget indexing), this is a concrete path to Liberado accumulating its own operating manual.
The mem0 dependency itself is not worth carrying — Liberado already has a sovereign vector store.

## Follow-up hooks (not done here)
- Draft the delegation-gate capability against Liberado's policy model (a `coding`-pack design task).
- Add auto-index-on-append to the `liberado-qdrant-mcp` roadmap, with an `index_failures` ledger.
- Once these are captured in Liberado's own roadmap, `subagent-manager-mcp-master/` can be archived.
