---
kind: finding
status: active
authority: evidence
domain: security
canonical_for: vault-write-gate-audit
open_items: true
---

**Status:** Shipped in PR (post #295). Update 2026-10-12: the multi-path destination-zone check
(`move_note`/`move_file` `from`+`to`) is now in, and the VaultWide gate requires the explicit
marker. The Open items at the bottom reflect the new state.

# MCP write paths not covered by the vault-zone gate

This document audits every other MCP write path that reaches external state without
the gate asking a human. The fix in this branch closes the path-addressed
`turbovault:*` hole (F1 + live bug `01M3ZB89WK54HGNG6ATJ9FPJMX`); this audit lists
the rest so each one can be classified as "covered by the same chokepoint",
"covered by a different gate", or "open follow-up".

## TL;DR

- **Covered by the same chokepoint** (the runtime gate now resolves EVERY tool on a
  path-addressed MCP through `WriteTarget`, including vaults-wide mutators and multi-path
  tools): the bug class that produced F1 is structurally fixed. A new tool without a
  classification is refused with an actionable message; a `move_*` with one zone in the grant and
  another not raises a permission request naming the missing zone; a vault-wide tool without the
  explicit marker grant refuses (no more "any Write is enough" weak gate).
- **Covered by a different gate**: the non-vault MCPs (calorie-counter, actual,
  caldav, …) declare `writes_vault = false` in the deployed topology, so the per-zone
  check returns `NotAWrite`. The runtime gate's *consequence*, *magnitude*, and
  *capability* checks still apply — those are the protections in force today.
- **Open follow-ups**: see "Open items" at the bottom. None is critical on the
  deployed nodes.

## Audit table

Each entry: the MCP, the writes that reach state, the gate(s) the runtime applies
today, and the residual risk if any.

### 1. `liberado-actual-mcp` (Actual Budget)

- **Writes**: `create_category`, `set_budget_amount`, `import_csv`,
  `create_transfer`, every transaction-modifying call.
- **Topology**: `liberado-actual-mcp` declares `writes_vault = false` and
  `consequence = "external"`.
- **Gate(s) in force**:
  - **Capability**: the agent must hold `ExecuteMcp("liberado-actual-mcp")`.
    A `main-agent` profile (the human chat) does **not** hold it (the dispatcher
    grant does), so a chat agent cannot reach Actual Budget directly.
  - **Consequence**: `external` (rated in `deploy/homelab/config/topology.toml`).
    The consequence gate downgrades EVERY call to a proposal — the human MUST
    approve a high-consequence MCP's call before it runs.
  - **Magnitude**: covered as well, but consequence fires first.
  - **Zone**: `NotAWrite` (the MCP declares `writes_vault = false`).
- **Verdict**: **covered**, no human approval except via the proposal card.
  Actual is rated `external` and that rating is honest (writes a remote
  budget ledger).

### 2. `liberado-caldav-mcp` (CalDAV)

- **Writes**: `create_event`, `update_event`, `delete_event` (calendar writes —
  external effect, but reversible).
- **Topology**: `writes_vault = false`; `consequence = "reversible"`.
- **Gate(s) in force**:
  - **Capability**: must hold `ExecuteMcp("liberado-caldav-mcp")`. A `main-agent`
    profile doesn't; a delegated life or dispatcher session does.
  - **Consequence**: `reversible` (below the `Irreversible` gate).
  - **Magnitude**: scans call arguments for sweeping phrasing; a `delete_event`
    with one ID is bounded.
  - **Zone**: `NotAWrite`, no per-zone check.
- **Verdict**: **covered, no human approval card**. A strictly read-only
  `main-agent` chat cannot reach it (no ExecuteMcp grant); a `life` or
  `dispatcher` session can call `create_event` directly with no card.
  Operator-discretion judgement: route through dispatcher + magnitude waiver
  if every calendar write should require a card.

### 3. `liberado-calorie-counter-mcp` (food log)

- **Writes**: `log_food`, `delete_log_entry`, `set_goals`. All reversible
  (history is append-only).
- **Topology**: `writes_vault = false`; `consequence = "reversible"`.
- **Gate(s) in force**: capability + consequence (`reversible`, below the gate)
  + magnitude. **No human approval card** today.
- **Verdict**: **covered**. A delegated life session can call these
  directly without an approval card; they should not need one given the
  data is personal and reversible.

### 4. `liberado-spider-mcp` (web scrape)

- **Writes**: none — `read_only` consequence. Out of scope.

### 5. `liberado-qdrant-mcp` (semantic memory)

- **Writes**: `add_to_collection`, `delete_from_collection`,
  `update_collection_config` (writes to a local vector store; not a vault zone).
- **Topology**: `writes_vault = false`; `consequence = "reversible"`.
- **Gate(s) in force**: capability + consequence.
- **Verdict**: **covered, no human approval**. Operator call: raise to
  `external` if every add/delete should be gated.

### 6. `liberado-pdf-mcp`

- **Writes**: `merge_pdf`, `split_pdf`, … — file IO outside the vault.
- **Topology**: `writes_vault = false`; `consequence = "reversible"`.
- **Verdict**: **covered**.

### 7. `liberado-anythingllm-mcp`

- **Writes**: ingests into AnythingLLM workspace.
- **Topology**: `writes_vault = false`; `reversible`.
- **Verdict**: **covered**.

### 8. `liberado-python-interpreter-mcp`

- **Writes**: `pip install`, file scratch. The interpreter runs as a
  persistent REPL with workspace volume; `reversible` is technically right
  (container restart wipes the volume) but a `pip install` of a malicious
  package is *not* "reversible from the user's perspective".
- **Verdict**: **covered** by consequence (below the gate). Operator call.
  Promote to `external` if escape becomes a concern.

### 9. Workspace tools (not MCPs — runtime-internal): `workspace_write`, `workspace_delete`

- **Writes**: write/delete files in `agent_workspace` (per-agent scratch
  directory; see `[agent_workspace]` in tuning spec).
- **Gate**: cap (`max_bytes`, `0` refuses every write), path-escape checks,
  and the magnitude gate reads the arguments for sweeping phrasing.
- **Verdict**: **covered** by the workspace cap + magnitude heuristic. The
  agent_workspace is explicitly a scratch area.

### 10. `create_agent` — creating Agents-shelf chats

- **Writes**: spawns a new chat session with the named profile.
- **Gate**: operator profile (`operator`) grants this as an MCP-server-wide
  capability; the `chat-default` profile does not.
- **Verdict**: **covered by capability**. The chat-create capability is
  granted to `operator` and only `operator`; no profile accidentally
  inherits it.

### 11. `turbovault` tools NOT in the fix's classification

The fix classifies every tool the live turbovault currently exposes
(see `deploy/homelab/config/topology.toml`): all path-addressed writers
(single and multi-arg), all reads, all vault-wide mutators (the latter gated
by the explicit marker grant). New tools added by an upgrade must be
classified at deploy time (else the operator's first call after upgrade
gets the undeclared-tool refusal, which is the desired fail-closed behavior).
The deploy-time discipline is:

1. After upgrading the turbovault image, run `tools/list` against the new server.
2. For each new tool, decide which class it falls into:
   - `write_tools` (mutates one or more zones — declare `write_path_args` if the
     path arg is named differently or there are multiple).
   - `read_tools` (a read).
   - `vault_wide_writes` (mutates vault state without a zone; needs the
     explicit marker grant to call).
3. Update BOTH `deploy/homelab/config/topology.toml` AND
   `services/liberado/config/topology.toml` on the live node before restarting
   the daemon.
4. Config validation refuses to boot if any of these lists has internal
   overlap (a tool in two lists is silently demoted to the first class that
   matches) or a `write_path_args` key is missing from `write_tools`.

## Open items

- **`liberado-python-interpreter-mcp` consequence rating.** Rated
  `reversible` despite holding network egress (`pip install`). Operator
  call. Promote to `external` if escape becomes a concern.

- **Live-node topology parity.** `services/liberado/config/topology.toml`
  is the deployed topology on the actual node (vs.
  `deploy/homelab/config/topology.toml` which is the homelab Compose). Both
  must carry the new `read_tools` / `vault_wide_writes` / `write_path_args`
  entries. The deploy notes file in the branch names this gap explicitly so
  the box update task is mechanical.

- **`liberado-actual-mcp` approval friction.** Rated `external`, so every
  call requires an approval card. A session that just wants to read its
  spend (transaction list, balance) hits the consequence gate anyway
  because the catalogue doesn't track per-tool read/write. Operator may
  want to model Actual as `reversible` with a per-tool list of writers.

- **`liberado-caldav-mcp` / `liberado-calorie-counter-mcp` approval
  gap.** A `life` session can call `create_event` / `log_food` without an
  approval card. If the operator wants those gated, the fix is the
  consequence rating (move to `external`) — there's no per-zone
  classification available because the MCPs declare `writes_vault = false`.

- **`create_agent` authority.** The `operator` profile is the only one that
  grants it, but every model the operator ever wants to spawn on demand
  inherits it implicitly. A future tightening could split this into
  `create_agent(limit)` or `create_agent(profile = …)` so the operator can
  spawn a researcher but not a `dispatcher`-spawning profile.

- **Per-zone listing for non-vault MCPs.** Once the operator decides a
  calorie-counter `log_food` should require approval, the only mechanism
  available is the consequence rating (raise it to `external`). A future
  patch could let `liberado-calorie-counter-mcp` declare per-tool
  zones — e.g. `log_food: zone = "food_log"` — and the gate would
  handle it like any other vault write (subject to `Write(Vault("food_log"))`).

- **`01M3ZB89WK54HGNG6ATJ9FPJMX` retrospective.** The `conformance` zone is
  `agent_writable` in `deploy/homelab/config/policy.toml`. Worth a
  discussion: should `conformance` be `human_only` to harden the conformance
  probe pool against an agent that drifts? Today it relies on a per-tool
  profile grant.
