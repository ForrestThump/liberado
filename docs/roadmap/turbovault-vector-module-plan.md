# TurboVault `vector` module — integration roadmap

Status: **prototype shipped + live on Liberado homelab (2026-07-19).** Architecture
decisions below are locked and implemented on fork `develop` (Phases 1–4 of the
[prototype plan](turbovault-vector-prototype-plan.md)). Remaining work is upstream
curation (#42 / #43 / module split PRs), multi-vault hardening, and freshness
graduation toward `vault_events` — not greenfield design.

Companion to `turbovault-modules-integration-roadmap.md`.

## Goal

A compiled-in, default-off **`vector`** module giving TurboVault semantic search
over the vault, that is:

- **Customizable** — pluggable embedder (local default, remote/API optional),
  tunable chunking + hybrid weights, via `read_config` / module config.
- **Loosely coupled** — embedded by default (no external service); depends only
  on the curated plugin boundary + a shared engine crate.
- **Incremental** — on a note change, re-embeds only the **changed chunks**, not
  the whole note or the whole vault.

## Decisions (locked)

1. **Embedded native engine is the default/required path**; **Qdrant is an
   optional cargo feature** a user may enable, never the default or a hard dep.
2. **Plugin state lives under `.turbovault/plugins/<name>/`.** Precedent already
   exists on `main` (`.turbovault/audit/`) and on the vector feature branch
   (`.turbovault/vectors/state.db`). Propose to Nick that the host creates
   `.turbovault/plugins/<id>/` on init for each compiled-in plugin and hands the
   module its path (small new capability — see "Proposals to Nick").
3. **No dependency on `lqm-core`** (would force life-os public). Instead **promote
   `turbovault-vector` to a first-class in-tree TurboVault crate** — it is already
   the shared engine (life-os `memory-store` path-depends on
   `turbovault/crates/turbovault-vector`). Optionally **port** select pure
   functions from `lqm-core` (below), copied not depended-on. Revisit making
   life-os public only if duplication ever justifies it.
4. **Build on `turbovault-vector`** (embedded, incremental, hybrid+rerank),
   already battle-tested by life-os `memory-store`. Wire its **incremental
   `IndexBuilder::update_file`** to note changes — the missing piece — instead of
   `full_rebuild`.

## Sources & reuse map

| Source | What it is | Use |
|---|---|---|
| **`turbovault-vector`** (fork `feature/vector-db`) | Embedded: `fastembed` + `usearch` (HNSW/mmap) + SQLite sidecar. `IndexBuilder` (`update_file` = **chunk-level incremental** via `diff_chunks`; `full_rebuild`), `SearchRouter` (**hybrid BM25+vector + optional cross-encoder reranker**), `VectorIndex`, `ChunkStore`, `EmbeddingEngine` trait. | **Core engine** — promote in-tree, build the module on it |
| **`lqm-core`** (liberado-qdrant-mcp) | Qdrant RAG engine. `Embedder` trait (fastembed/**ollama**/**openai**), RRF fusion + sparse TF, **MMR** rerank, chunk **reconstruction/neighbor-expansion**, **scope/clearance**. | **Idea/code ports** (copy pure fns), not a dep. Optional Qdrant backend reference |
| **life-os `memory-store`** | Wraps `turbovault-vector`, drives `update_file` incrementally per add/delete. | **Reference wiring** for correct incremental use |

## The incrementality story (premise resolved)

Chunk-level incremental **already exists and is proven** — it is simply **not
wired on the TurboVault side**:

- `turbovault-vector::IndexBuilder::update_file` embeds only changed chunks,
  reuses unchanged chunk vectors, deletes removed ones (`diff_chunks`).
- life-os `memory-store` calls `update_file` on every change → proven.
- But the TurboVault MCP feature branch only ever calls **`full_rebuild`**
  (whole-vault) at `tools.rs:3829`; nothing wires `update_file` to note writes.

**So the work is wiring, not invention.**

## The change-detection problem (open — needs a decision)

For incremental to fire, the module must know **which note changed**, including
**external Obsidian edits** and **core-tool writes** — not just plugin writes.

Findings:
- The **`HookBus` only fires on plugin writes** (`plugin_host.rs:186`). Core-tool
  writes and external edits do **not** publish to it.
- A **file watcher exists** (`turbovault-vault/src/watcher.rs`, notify-based) but
  emits `VaultEvent` over an **mpsc channel, not the HookBus** — the two systems
  are unconnected, and the watcher may not even be started by the MCP server.
- TurboVault's own change strategy is **mtime-based re-parse on query**
  (`manager.rs`, handles "git sync, direct writes, other processes").

Options:
- **A — lazy mtime reindex (self-contained).** Mirror TurboVault: on query (or a
  cheap periodic tick), find notes whose mtime ≠ stored mtime and `update_file`
  them. Needs cheap mtime access — `VaultApi` exposes only paths + a content-hash
  version (reading every note to hash is expensive). ⇒ likely a tiny `VaultApi`
  addition exposing note mtimes (or `list_notes_meta`).
- **B — comprehensive HookBus (propose to Nick).** Wire the existing
  `VaultWatcher` **and** core writes to publish `HookEvent`s. Real-time
  incremental; the module just subscribes. **Also the foundation the
  `vault_events` module needs** — shared investment.
- **C — hybrid.** HookBus for real-time where available + lazy mtime fallback.

Leaning: **B** (it unblocks both `vector` and `vault_events`), with **A** as the
no-API-change fallback. TBD together.

## Proposed module architecture

- New crate **`turbovault-plugin-vector`** (`crates/plugins/`), compiled-in behind
  a default-off **`vector`** feature (mirrors `tasks`).
- Depends on **`turbovault-vector`** (promoted in-tree) + `turbovault-plugin-api`.
- **Embedder:** `turbovault-vector::EmbeddingEngine` — fastembed local default;
  remote/API optional. Consider porting `lqm-core`'s ollama/openai backends.
- **Storage:** `VectorIndex` (usearch) + `ChunkStore` (SQLite) under
  `.turbovault/plugins/vector/`.
- **Incremental:** `update_file` (chunk-diff) per changed note.
- **Search:** `SearchRouter` (hybrid BM25 + vector, optional reranker). Optional
  ports from `lqm-core`: RRF fusion, MMR diversification, neighbor-expansion
  reconstruction.
- **Tools (namespaced `vector_*`):** `vector_search`, `vector_reindex` (explicit
  full rebuild), `vector_status`, `vector_config`.
- **Customization** (`read_config` + module config): embedder backend + model,
  chunk size/overlap, hybrid weight, reranker on/off, qdrant on/off.
- **Change trigger:** per the change-detection decision above.
- **Qdrant (optional feature):** an alternate backend behind the `qdrant` feature;
  disabled by default.

## Prerequisite issues to settle with Nick (the two gates)

Both are **host capabilities the module cannot provide for itself**, so they are
proposed to TurboVault (issue → likely PR) and settled **before** the build.

### Issue 1 — a reliable change-detection signal (possibly extend the HookBus) — SUBMITTED [#43](https://github.com/Epistates/turbovault/issues/43), advocates C

The module must know **which note changed**, including external Obsidian edits and
core-tool writes — which the current `HookBus` does not surface (plugin writes
only). Present the options and recommend a direction:

- **A — `VaultApi` mtime accessor + lazy reindex** (smaller): expose note mtimes
  (e.g. `list_notes_meta`) so the module cheaply finds changed notes and calls
  `update_file`. No HookBus change.
- **B — comprehensive `HookBus`** (recommended): wire the existing `VaultWatcher`
  and core-tool writes to publish `HookEvent`s, making the bus the universal
  change feed. Bigger, but **shared foundation with the `vault_events` module** —
  so the cost is amortized across two modules.
- **C — hybrid.**

Recommendation: **B**, because it unblocks `vault_events` too; **A** as fallback.

### Issue 2 — persistent, plugin-private storage (`.turbovault/plugins/<id>/`) — SUBMITTED [#42](https://github.com/Epistates/turbovault/issues/42)

The vector index needs **read/write** persistence outside Obsidian. Proposal: the
host creates `.turbovault/plugins/<plugin_id>/` on init (when the plugin is
compiled in) and hands the module its **absolute path** (e.g. on `PluginContext`
or via a `state_dir()` accessor). The plugin reads and writes freely **within its
own assigned directory** — nothing else.

- Distinct from `read_config` (which is **read-only** and scoped to Obsidian's
  `.obsidian/`). This is a **read-write, plugin-owned** state dir under
  `.turbovault/`.
- Precedent already exists: `.turbovault/audit/`, and the vector feature branch's
  `.turbovault/vectors/state.db`.
- **Smaller and less contentious than Issue 1** — likely a quick yes; may land
  first.

### Not a gate — promote `turbovault-vector` in-tree

Moving `turbovault-vector` from the feature branch to a maintained in-tree crate
is an **implementation/PR** step (part of shipping the module), not a capability
question — so it can proceed in parallel with the two issues above.

## Implementation scope (best-case: #41/#42/#43 land as proposed)

### Engine — promote `turbovault-vector` in-tree

- Lift `crates/turbovault-vector/` (fork `feature/vector-db`) onto TurboVault main
  as a maintained, default-off crate. **Leave behind** the branch's monolith
  integration (`tools.rs` +1077, `manager.rs`, `models.rs`, batch, file_tools) —
  the plugin replaces all of it.
- **Public API is clean and reusable as-is:** `EmbeddingEngine` / `FastembedEngine`,
  `Reranker` / `FastembedReranker`, `ChunkStore`, `VectorIndex`, `IndexBuilder`
  (`full_rebuild` / `update_file`), `SearchRouter` (hybrid BM25+vector + rerank),
  `VectorConfig`, `VectorError`.
- **Dep cleanup:** the only real deps are `turbovault-parser` (`to_plain_text`) and
  `turbovault-core` (one config type — replace with the crate's own `VectorConfig`).
  `turbovault-vault` appears **unused → drop**. Remaining deps are stable on main.
  Features `local` (default: fastembed + usearch + rusqlite) / `remote` (reqwest).
- **The one real refactor — a content-fed API.** Today `update_file(path, root)` and
  `full_rebuild(root)` **read files / walk the filesystem** (`tokio::fs`, `walkdir`),
  but a plugin gets content via `VaultApi` (relative paths, no root). Extract a
  content-fed core:
  - `update_note(rel_path, content)` — chunk → hash-diff → embed only new chunks.
  - `remove_note(rel_path)` — drop chunks for a deleted note.
  - Keep `update_file` / `full_rebuild` as **thin filesystem wrappers** over these,
    so **life-os `memory-store` (which path-deps this crate) keeps building
    unchanged**, and can adopt the content-fed API later.
  - The plugin then never needs the vault's filesystem root — all index I/O stays
    inside its own `plugin_state_dir`.
- **Side benefit:** once on main, life-os's `turbovault-vector` path-dep no longer
  requires the fork's feature branch to be checked out.

### Module — `turbovault-plugin-vector`

- New `crates/plugins/turbovault-plugin-vector`, default-off host `vector` feature
  (mirrors `tasks`). Deps: `turbovault-vector` + `turbovault-plugin-api`.
- **Construction:** `Plugin::build` is sync, but engine setup (load embedder, open
  index) is async/heavy → lazy `OnceCell` on first use (same pattern as tasks'
  config). Uses `VaultApi::plugin_state_dir()` (#42) to open `VectorIndex` +
  `ChunkStore` under `.turbovault/plugins/vector/`, and `read_config` (#41) to tune
  model/backend, chunk size/overlap, hybrid weight, rerank on/off.
- **Change-feed wiring (#43 option C):**
  - Subscribe to `HookBus`: `FileCreated/Modified/Renamed` → `read_note` →
    `update_note`; `FileDeleted` → `remove_note`.
  - Reconcile task: at startup, periodically, and on `HookRecvError::Lagged`, use the
    mtime accessor → diff vs `ChunkStore` mtimes → `update_note` the changed ones.
    Push for speed, pull for correctness.
- **Tools (namespaced `vector_*`):** `vector_search` (hybrid + optional rerank),
  `vector_reindex` (explicit full rebuild), `vector_status` (notes/chunks/model/dims/
  last-reconcile), `vector_config` (resolved settings + source, like `tasks_config`).
- **Concurrency:** `Arc<RwLock<VectorIndex>>` (the memory-store pattern).

### Notes / risks

- **Model download:** fastembed fetches ONNX models on first use (tens–hundreds of
  MB) → cache under `plugin_state_dir`; first-search latency (lazy load) — surface in
  `vector_status`.
- **Per-vault:** the index is per active vault (via #42's per-vault state dir); on
  active-vault switch, open the right handle (key engine handles by vault).
- **Embedder backends:** fastembed (local) default; the crate's `remote` feature for
  API embedding; ollama/openai ports from `lqm-core` deferred unless wanted.

### Suggested PR sequence (after the issues land)

1. **PR: add `turbovault-vector` crate** — promoted, dep-cleaned, content-fed API,
   default-off. Reviewable in isolation, no plugin. **← DONE (groundwork), fork
   branch `feat/vector-engine-crate`, not yet opened.** Built off `upstream/main`;
   dropped `turbovault-core`/`turbovault-vault` deps (owns `VectorConfig`); added
   `update_note`/`remove_note` with `update_file`/`full_rebuild` as wrappers; 34
   tests pass (incl. 3 new content-fed); fmt+clippy clean. Verified: memory-store's
   turbovault-vector API usage is preserved (life-os's *other* build errors are an
   unrelated turbovault 1.5→1.6 `turbovault-vault` gap in `liberado-vault`).
   Known pre-existing nit: `--no-default-features` compile (a gated `PathBuf`
   import) — orthogonal to this work; default build is clean.
2. **PR: `turbovault-plugin-vector` module** — wiring, tools, tests (unit + host
   integration + fail-closed live-vault, like `tasks`).
3. Optional follow-ups: ollama/openai backends; `qdrant` feature.

## Open decisions (to settle together)

- [ ] Change-detection approach: **A / B / C** above.
- [ ] Scope of `lqm-core` ports (RRF, MMR, neighbor-expansion, ollama/openai) vs.
  keep lean on `turbovault-vector`'s existing router.
- [ ] Reconcile any drift between the fork's `turbovault-vector` and the version
  life-os builds against (they resolve to the same path today).
- [ ] Tool surface + response shapes for `vector_*`.
- [ ] Sequencing vs. `vault_events` (shared HookBus foundation may reorder them).

## Sequencing

**Gate: settle the two prerequisite issues before building the module** — so the
module is built against agreed, stable host contracts rather than guesses.

1. Land `tasks` (#41 read_config first, then the tasks module).
2. **Open Issue 2 (plugin state dir)** — small, likely fast.
3. **Open Issue 1 (change-detection / HookBus)** — recommend option B; coordinate
   with `vault_events` since it shares the foundation.
4. *In parallel (not gated):* promote `turbovault-vector` in-tree and prototype
   wiring `update_file` to the incremental path.
5. Once both issues are settled: build `turbovault-plugin-vector` (search +
   reindex + config) on the plugin boundary, using the assigned state dir and the
   agreed change signal; port `lqm-core` niceties as chosen.
6. Optional Qdrant feature.

Note: Issue 2 and Issue 1 may land on different timelines (2 is smaller); the
build proper waits on both, but step 4 keeps progress moving in the meantime.
