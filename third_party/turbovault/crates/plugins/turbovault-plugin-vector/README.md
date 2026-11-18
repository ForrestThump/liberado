# turbovault-plugin-vector (prototype)

Semantic vector search for TurboVault as a **compiled-in, default-off plugin
module**. Wraps the embedded `turbovault-vector` engine (fastembed + usearch +
SQLite) behind the plugin boundary.

```bash
cargo run -p turbovault --features vector
```

Tools (advertised namespaced under `vector`):

| Tool | Purpose |
|---|---|
| `vector_search`  | semantic search; returns similar note chunks |
| `vector_reindex` | force a full re-scan into the index |
| `vector_status`  | model, dimensions, indexed-note count, chunking |
| `vector_config`  | resolved configuration + source |

The index persists in a per-vault directory under
`$XDG_STATE_HOME/turbovault/plugins/vector/<vault>` (`~/.local/state/...` when
`XDG_STATE_HOME` is unset). Notes are read through `VaultApi`; the index is kept
current by comparing each note's version token from `read_note` and calling
`update_note`, which re-embeds only changed chunks.

**Prototype scope:** reconcile-on-demand (before search / on reindex) rather than
a real-time hook-subscription task; single active vault; default config. The host
API does not yet expose a vault-local state directory or note mtimes.
