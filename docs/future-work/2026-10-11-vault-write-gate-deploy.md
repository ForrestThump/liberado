# Deploy notes — `fix/vault-write-gate` (2026-10-11)

**Status:** Changes ready to ship. The runtime fix and config wiring are
implemented in this branch; the operator action needed at deploy time is
listed here so the box update is mechanical.

## What ships in the runtime

- `RiskGatedToolRuntime::invoke` resolves every tool on a path-addressed
  MCP through `WriteTarget` (`NotAWrite` / `Zone` / `VaultWide` /
  `Undeterminable`).
- A tool NOT in any of `write_tools` / `read_tools` / `vault_wide_writes`
  on a path-addressed MCP fails CLOSED: the call is refused with a
  refusal that names the three lists the operator must classify it
  under. The live bug (`01M3ZB89WK54HGNG6ATJ9FPJMX` — `edit_note` on
  `conformance/...`) is fixed at the type level.
- Vault-wide mutators (`begin_fanout`, `commit_fanout`, `abandon_fanout`,
  `set_active_vault`, `vector_reindex`, ...) require the grant to hold at
  least one `Write(Vault(\"<zone>\"))` capability, and surface the same
  Deny/Once/Session/Everywhere permission-request flow as a per-zone
  write.
- Config validation refuses to boot if any tool is listed in two of the
  three lists (the runtime resolves in a fixed order, so an overlap
  silently demotes the tool to a less-strict class).

## Operator action — update topology files

The runtime change is fail-closed, which means a deployed topology
file that does not classify every tool will REFUSE calls rather than
silently allow them. The deployed turbovault has 70+ tools today;
rebooting the daemon without an updated topology.toml will start
failing safe (refusals), but the operator will see them as broken
calls.

The required topology file change is:

### Files to update

1. **`deploy/homelab/config/topology.toml`** — already updated in this
   branch. The `turbovault` entry has full `read_tools`,
   `vault_wide_writes`, and an expanded `write_tools` covering the
   task tools (renamed from `update_task` / `delete_task`).

2. **`services/liberado/config/topology.toml`** — the live node's
   deployed topology. NOT in this repo (it's on the box). Must carry
   the SAME lists. The deploy step:

   ```sh
   # On the box, after pulling this branch:
   # Mirror the homelab topology changes into the live node.
   diff deploy/homelab/config/topology.toml services/liberado/config/topology.toml
   # Apply the same read_tools / vault_wide_writes lists to the turbovault entry.
   ```

3. **`config.example/topology.toml`** — already updated in this
   branch with a commented-out example showing the three-list shape.

## Verification before deploy

Once the topology files are in place, the daemon refuses to boot if
the lists overlap. A successful boot proves the operator's
classification is internally consistent.

```sh
liberado validate                              # or `cargo run -p liberado-cli -- validate`
```

The output lists every MCP and which classification shape it uses.
For `turbovault`, expect: `path-addressed: write_tools=N read_tools=N vault_wide_writes=N`
where N is the count of tools in each list.