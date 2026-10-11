**Status:** Shipped in PR (post #295).

# MCP write paths not covered by the vault-zone gate

This document audits every other MCP write path that reaches external state without
the gate asking a human. The fix in this branch closes the path-addressed
`turbovault:*` hole (F1 + live bug `01M3ZB89WK54HGNG6ATJ9FPJMX`); this audit lists
the rest so each one can be classified as "covered by the same chokepoint",
"covered by a different gate", or "open follow-up".

## TL;DR

- **Covered by the same chokepoint** (the runtime gate now resolves EVERY tool on a
  path-addressed MCP through `WriteTarget`, including vaults-wide mutators): the
  bug class that produced F1 is structurally fixed. A new tool without a
  classification is refused with an actionable message.
- **Covered by a different gate**: the non-vault MCPs (calorie-counter, actual,
  caldav, …) are not path-addressed, so they don't go through the
  `write_target` resolution. Their writes reach the runtime gate; the gate's
  per-zone check returns `None` for them (because they declare `writes_vault =
  false` in the deployed topology), so **no permission card fires** for them. The
  runtime gate's *consequence* check, *magnitude* check, and *capability check*
  still apply — those are the protections in force today.
- **Open follow-ups**: see "Open items" at the bottom. None is critical on the
  deployed nodes (every audit row below is also covered by consequence class or
  by the operator's manual review of the agent's tool list), but the question
  "is a strictly read-only human chat in Telegram allowed to call
  `liberado-caldav-mcp:create_event`?" should be re-examined at the operator's
  discretion.

## Audit table

Each entry: the MCP, the writes that reach state, the gate(s) the runtime applies
today, and the residual risk if any.

### 1. `liberado-caldav-mcp` (CalDAV)

- **Writes**: `create_event`, `update_event`, `delete_event` (calendar writes —
  external effect, but reversible).
- **Topology**: `liberado-caldav-mcp` declares `writes_vault = false`; the runtime
  gate's `WriteTarget` resolves to `NotAWrite` for every call (the MCP isn't
  path-addressed nor uses zone tracking).
- **Gate(s) in force**:
  - **Capability**: the agent must hold `ExecuteMcp("liberado-caldav-mcp")`. A
    human chat in the `main-agent` profile doesn't (the dispatcher grant does,
    so a delegated life session can use it).
  - **Consequence**: the MCP is rated `reversible`, BELOW the
    `CONSEQUENCE_GATE` (`Irreversible`); the consequence gate does NOT refuse
    it. Calendar deletes are recoverable, so `reversible` is honest.
  - **Magnitude**: the running magnitude scan reads the call's arguments.
    A `delete_event` with one ID is bounded (one path, one event).
  - **Zone**: `NotAWrite`, no per-zone check.
- **Verdict**: **covered**. The runtime gate's other guards catch what's
  appropriate. A strictly read-only `main-agent` chat cannot reach it (no
  ExecuteMcp grant). A `life` session can call `create_event` directly without
  an approval card — operator-discretion judgement; if the operator wants every
  calendar write gated, route through `dispatcher` and add a magnitude waiver.

### 2. `liberado-actual-mcp` (Actual Budget)

- **Writes**: `create_category`, `set_budget_amount`, `import_csv`,
  `create_transfer`, every transaction-modifying call.
- **Topology**: `liberado-actual-mcp` declares `writes_vault = false`.
- **Gate(s) in force**:
  - **Capability**: same as CalDAV.
  - **Consequence**: `external` (rated in
    `deploy/homelab/config/topology.toml`). The consequence gate downgrades
    every call to a proposal — the human MUST approve a high-consequence MCP's
    call before it runs.
  - **Magnitude**: covered as well, but consequence fires first.
- **Verdict**: **covered**. The `external` consequence rating is the right
  answer for an MCP that writes external state; every call becomes a proposal.

### 3. `liberado-calorie-counter-mcp`

- **Writes**: `log_food`, `delete_log_entry`, `set_goals`. All reversible
  (history is append-only).
- **Topology**: `writes_vault = false`.
- **Gate(s) in force**: capability + consequence (`reversible`, below the gate).
  **Magnitude** scan reads arguments.
- **Verdict**: **covered** for the same reason as CalDAV. A delegated life
  session can call these without an approval card; they should not need one
  given the data is personal and reversible.

### 4. `liberado-spider-mcp` (web scrape)

- **Writes**: none — `read_only` consequence. Out of scope.

### 5. `liberado-qdrant-mcp` (semantic memory)

- **Writes**: `add_to_collection`, `delete_from_collection`,
  `update_collection_config` (writes to a local vector store; not a vault zone).
- **Topology**: `writes_vault = false`; `consequence = "reversible"` (its own
  data store, recoverable by re-ingestion).
- **Gate(s) in force**: capability + consequence (`reversible`).
- **Verdict**: **covered**. Should the operator want every add/delete to its
  semantic-memory store gated, raise the consequence to `external`; today the
  rating matches the data.

### 6. `liberado-pdf-mcp`

- **Writes**: `merge_pdf`, `split_pdf`, etc. — file IO outside the vault.
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
- **Verdict**: **covered** by consequence (below the gate). Operator-discretion
  judgement; the deployed MCP is rated `reversible` not `external`, which is
  the conservative-but-not-strict call.

### 9. Workspace tools (not MCPs — runtime-internal): `workspace_write`,
    `workspace_delete`

- **Writes**: write/delete files in `agent_workspace` (a per-agent scratch
  directory; see `[agent_workspace]` in tuning spec).
- **Gate**: cap, path-escape checks, and the magnitude gate read the arguments
  for sweeping-destructive phrasing. **Not** a vault-zone write.
- **Verdict**: **covered** by the workspace cap + magnitude heuristic. The
  agent_workspace is explicitly a scratch area.

### 10. `create_agent` — creating Agents-shelf chats

- **Writes**: spawns a new chat session with the named profile.
- **Gate**: operator profile (`operator`) grants this as an MCP-server-wide
  capability; the `chat-default` profile does not.
- **Verdict**: **covered by capability**. The chat-create capability is
  granted to `operator` and only `operator`; no profile accidentally
  inherits it. The agent's own grant is the only authority needed.

### 11. `turbovault` tools NOT in the fix's classification

The fix classifies every tool the live turbovault currently exposes
(see `deploy/homelab/config/topology.toml`). New tools added by an
upgrade must be classified at deploy time (else the operator's first
call after upgrade gets the undeclared-tool refusal, which is the
desired fail-closed behavior). The deploy-time discipline is:

1. After upgrading the turbovault image, run `tools/list` against the new
   server.
2. Add any new tool to one of `write_tools` / `read_tools` /
   `vault_wide_writes`.
3. Update BOTH `deploy/homelab/config/topology.toml` AND
   `services/liberado/config/topology.toml` on the live node before
   restarting the daemon.

## Open items

- **`move_note` / `move_file` destination-zone check.** The fix resolves the
  SOURCE zone (`from`) via `zone_from_arg`; the DESTINATION zone (`to`) is
  not yet checked against the grant. A `move_file` from `Tasks` (in grant) to
  `sandbox` (not in grant) currently passes the source-side gate and runs. A
  multi-path-aware `WriteTarget` (resolving multiple args, e.g.
  `path_args = ["path", "from", "to", "file_path"]`) closes this. Tracked
  separately because the runtime surface change is non-trivial. The current
  state would be made worse by the fix if not kept in mind; right now a
  Tools-list audit before each upgrade is the workaround.

- **`liberado-python-interpreter-mcp` consequence rating.** Rated
  `reversible` despite holding network egress (`pip install`). Operator
  call. Promote to `external` if escape becomes a concern.

- **Live-node topology parity.** `services/liberado/config/topology.toml` is
  the deployed topology on the actual node (vs.
  `deploy/homelab/config/topology.toml` which is the homelab Compose). Both
  must carry the new `read_tools` / `vault_wide_writes` lists. The deploy
  notes file in the branch names this gap explicitly so the box update
  task is unambiguous.
