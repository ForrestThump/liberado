---
kind: runbook
status: draft
authority: advisory
domain: security
---

# Deploy notes — `fix/vault-write-gate` (2026-10-11)

**Status:** Changes ready to ship. The runtime fix and config wiring are
implemented in this branch; the operator action needed at deploy time is
listed here so the box update is mechanical.

## What ships in the runtime

- `RiskGatedToolRuntime::invoke` resolves every tool on a path-addressed
  MCP through `WriteTarget` (`NotAWrite` / `Zone` / `Zones(Vec<String>)` /
  `VaultWide` / `Undeterminable`).
- A tool NOT in any of `write_tools` / `read_tools` / `vault_wide_writes`
  on a path-addressed MCP fails CLOSED: the call is refused with a
  refusal that names the three lists the operator must classify it
  under. The live bug (`01M3ZB89WK54HGNG6ATJ9FPJMX` — `edit_note` on
  `conformance/...`) is fixed at the type level.
- Multi-path tools (`move_note(from, to)`, `move_file(from, to)`,
  `create_from_template(file_path)`, `generate_index(directory)`,
  `append_log_entry(directory)`, `visualize(output)`) now declare
  per-tool `write_path_args`, so every path-arg resolves to a zone and
  the runtime requires Write for **each** one. A mixed-zones move (one
  zone in grant, one not) raises a permission request naming the missing
  zone(s).
- Vault-wide mutators (`begin_fanout`, `commit_fanout`, `abandon_fanout`,
  `vector_reindex`) require the explicit marker grant
  `Capability::Write(Zone::vault("vault-wide-any"))`. A write to one zone
  is not enough. `set_active_vault` is admin: a session is hard-denied,
  and there is no approval card.
- Vector reads (`vector_search` / `vector_status` / `vector_config` —
  only `vector_reindex` rebuilds state) move to `read_tools` so a
  Read-only session can use them without a permission card.
- Config validation refuses to boot if any tool is listed in two of the
  three lists, or if a `write_path_args` key is missing from `write_tools`.

## Operator action — update topology files

Bundled TurboVault defaults classify every live tool when the MCP
config omits the lists. The old 8-tool `write_tools` block still boots.
Config may only extend or tighten those defaults. A tool the bundle
does not know is refused. Overlapping lists still refuse to boot.

The required topology file change is:

### Files to update

1. **`deploy/homelab/config/topology.toml`** — already updated in this
   branch. The `turbovault` entry has full `read_tools`,
   `vault_wide_writes`, and an expanded `write_tools` covering the
   task tools (renamed from `update_task` / `delete_task`).

2. **`services/liberado/config/topology.toml`** — the live node's
   deployed topology. NOT in this repo (it's on the box). The old
   8-tool block still works, because bundled defaults fill the lists.
   Copy the homelab lists when the classification should live in the
   file. The deploy step:

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