# TurboVault `vector` module â€” end-to-end prototype plan (fork)

Status: **Phases 1â€“4 DONE; LIVE on Liberado homelab (2026-07-19).**

Merged onto fork **`develop`** (was `prototype/vector-e2e`, stacked on the engine
+ read_config). Working end-to-end: `vector_search` over a real vault returns
sensible semantic results (query "deliberation and reasoning" â†’ the
`deliberation/*.md` notes at 0.717). **Liberado's live Telegram agent** reaches
these tools through the homelab TurboVault peer (`http://turbovault:3001`,
image built with `--features vector`). All prototype goal conditions met; fmt +
clippy clean; feature-off tree excludes the module.

**Phase 5 (update upstream issues/PRs) is still pending** manual review per the
autonomy boundary. Commits of record:
- `#42` per-vault `plugin_state_dir` (+ per-plugin VaultApi refactor)
- `#43` watcherâ†’HookBus bridge + `list_notes_meta`
- `turbovault-plugin-vector` module (`vector_search/reindex/status/config`)
- Phase-4 tests + a real chunking bug fix (char-boundary overlap, found by the
  live search).

Architecture/decisions live in `turbovault-vector-module-plan.md`; Liberado-side
"what's next" in [`current.md`](../current.md) and the modules
[umbrella](../turbovault-modules-integration-roadmap.md).

## Goal

A **working, end-to-end `vector` semantic-search module** on the fork â€”
engine + the two proposed host capabilities (#42 state dir, #43 change-feed) +
the plugin â€” so that:

1. A user can enable a default-off `vector` feature, and the daemon indexes the
   active vault, keeps the index current as notes change (incrementally,
   chunk-level), and answers `vector_search` queries.
2. The **working branch is proof**: we update issues #42/#43 and the PRs with
   links to it, demonstrating the proposed API shapes are real and usable
   (dogfooding â€” a real consumer is the strongest argument the shapes are right).

We have the whole fork to work with, so we prototype *everything* rather than
stubbing against hypothetical APIs.

## Approach & branch strategy

- Prototype the entire thing on one integration branch, stacked on the engine:
  - `feat/vector-engine-crate` â€” **the engine crate (done).**
  - `prototype/vector-e2e` â€” stacked on it; adds #42, #43, and the plugin. This
    is the working demo / proof branch.
- **Upstream PRs stay split and clean.** Once shapes are validated, cut focused
  branches from the prototype (`feat/plugin-state-dir`, `feat/hookbus-changefeed`,
  `feat/plugin-vector`) for the real PRs. The prototype branch is the proof, not
  necessarily the merge unit.

## Non-goals / prototype caveats

- End-to-end proof is prioritized over production hardening: watcher
  debounce/platform edge-cases, HookBus backpressure tuning, and multi-vault
  handle lifecycle may be rough and clearly marked `// prototype:`.
- Final upstream PRs will be split/cleaned to Nick's preferences; API shapes may
  change in review â€” expected, and exactly what dogfooding surfaces.
- Model download (fastembed ONNX) happens on first real embed; tests account for
  it and stay fail-closed.

---

## Phase 0 â€” Engine crate âœ… DONE

`feat/vector-engine-crate` (commit `7e47097`).

**Goal conditions (met):**
- [x] `turbovault-vector` in-tree, default-off workspace member; deps trimmed to
  `turbovault-parser`.
- [x] Content-fed `update_note` / `remove_note`; `update_file` / `full_rebuild`
  as wrappers (life-os `memory-store` unaffected).
- [x] 34 tests pass (incl. content-fed); fmt + clippy clean.

---

## Phase 1 â€” Capability: per-vault plugin state dir (#42)

**Goal:** the host hands the plugin a writable, per-vault, plugin-private
directory to persist its index.

**Work:**
- `VaultHost::plugin_state_dir()` (+ `VaultApi` passthrough) resolving
  `<active_vault_root>/.turbovault/plugins/<plugin_id>/`, `mkdir -p`, absolute
  path returned. Host bakes the `plugin_id` into each plugin's `VaultApi`/context.
- Ensure `.turbovault/` is gitignored (write `.turbovault/.gitignore` = `*`).

**Goal conditions (done when):**
- [ ] `plugin_state_dir()` returns `<vault>/.turbovault/plugins/<id>/`, created on
  demand, absolute.
- [ ] Distinct per `(vault, plugin)`; switching active vault returns a different
  path.
- [ ] A `ContractProvider`-style test writes a file under the returned dir and
  reads it back.
- [ ] Default (feature-off) build + existing tests unaffected.

---

## Phase 2 â€” Capability: change-feed (#43, option C)

**Goal:** the plugin learns of every note change (external edits, core-tool
writes, plugin writes) in real time, with a reconcile fallback for correctness.

**Key simplification:** the `VaultWatcher` watches the **filesystem**, so it
already sees all three change sources (everything lands on disk). The prototype
wires the watcher â†’ `HookBus` and adds an mtime accessor for reconcile â€” no
separate core-write hooks needed for the demo (so no dedup problem).

**Work:**
- Start a `VaultWatcher` for the active vault; forward `VaultEvent` â†’
  `HookEvent` onto the `HookBus` (reuse its `should_emit_event` filtering).
- `VaultHost::list_notes_meta() -> [(rel_path, mtime)]` (+ `VaultApi`
  passthrough) for the reconcile scan.

**Goal conditions (done when):**
- [ ] Editing a note on disk (external) emits a `HookEvent`; a subscriber test
  observes it.
- [ ] A core-tool write emits a `HookEvent` (watcher sees the fs change).
- [ ] `list_notes_meta()` returns per-note mtimes for the active vault.
- [ ] `Lagged` is surfaced to subscribers (so the plugin can trigger reconcile).

---

## Phase 3 â€” The `vector` plugin module

**Goal:** `turbovault-plugin-vector`, default-off, drives the engine over the
plugin boundary.

**Work:**
- New `crates/plugins/turbovault-plugin-vector`; host `vector` feature
  (default-off, mirrors `tasks`). Deps: `turbovault-vector` + `-plugin-api`.
- **Construction (lazy `OnceCell`):** open `VectorIndex` + `ChunkStore` under
  `plugin_state_dir()`; build `FastembedEngine` from `read_config` (model, chunk
  size/overlap, hybrid weight, rerank); `SearchRouter` (+ reranker if enabled).
- **Change handling:** subscribe to `HookBus` â†’ `Created/Modified/Renamed`:
  `read_note` â†’ `update_note`; `Deleted`: `remove_note`. Debounce per path.
- **Reconcile:** on first use, periodically, and on `Lagged`: `list_notes_meta`
  â†’ diff vs `ChunkStore` mtimes â†’ `update_note` the changed ones.
- **Tools (namespaced `vector_*`):**
  - `vector_search` (query, k) â†’ ranked `{path, score, preview}` (hybrid + rerank)
  - `vector_reindex` â†’ explicit full rebuild
  - `vector_status` â†’ indexed notes/chunks, model, dims, last reconcile
  - `vector_config` â†’ resolved settings + source (like `tasks_config`)

**Goal conditions (done when):**
- [ ] With `--features vector`, `vector_*` tools are advertised namespaced; absent
  otherwise; feature-off tree excludes the crate.
- [ ] Index persists in `.turbovault/plugins/vector/` across restarts.
- [ ] Editing one note re-embeds only its changed chunks (observable via
  `vector_status` counts / embed count).
- [ ] `vector_search` returns relevant notes for a query.

---

## Phase 4 â€” End-to-end verification

**Goal:** prove it works, on synthetic fixtures and a real vault.

**Work:**
- Host integration tests (feature-matrix like `tasks`): tools present/namespaced;
  feature-off catalog parity; a seed â†’ index â†’ search â†’ edit â†’ incremental-update
  round trip.
- Fail-closed, ignored, env-gated **live-vault test** over `~/Obsidian/Main`: real
  `fastembed` embeddings, index the vault, `vector_search` a known query and
  assert a plausible note surfaces.
- fmt + clippy clean (default and `vector` features).

**Goal conditions (done when):**
- [ ] Module unit + host integration tests green.
- [ ] Live-vault test indexes the real vault and returns sensible top-k (skips
  fail-closed when no vault / no model).
- [ ] fmt + clippy clean; feature-off base tool count unchanged.

---

## Phase 5 â€” Dogfood the proposals & close out

**Goal:** turn the working prototype into evidence on the issues/PRs.

**Work:**
- Push `prototype/vector-e2e` to the fork.
- Update **#42** and **#43** with links to the working branch + the specific
  files that implement/consume each capability, plus any **shape refinements**
  discovered while building the real consumer.
- Note on the engine PR (and the plugin PR when opened) that a full working
  consumer exists on the prototype branch.
- Prepare (or note as follow-up) the clean per-capability split branches for when
  Nick engages.

**Goal conditions (done when):**
- [ ] #42 and #43 each link the working branch and name the implementing +
  consuming code; any API adjustments are written up.
- [ ] Prototype branch pushed; engine/plugin PRs cross-reference it.

---

## Feasibility & autonomy notes (verified)

Checked the two things most likely to stall an autonomous run â€” both tractable,
no user decision required:

- **Phase 1 / `plugin_state_dir`:** the plugin `VaultApi` is currently **shared**
  across all plugins (`providers.rs:301` constructs one `VaultApi` before the
  plugin loop), so it can't identify the caller. **Fix:** move `VaultApi::new(...)`
  inside the `for plugin` loop and thread `descriptor.id` into
  `plugin_host::vault_host(...)` so `PluginVaultHost` carries the id. Small
  mechanical refactor.
- **Phase 2 / watcher:** `VaultWatcher::new(path, config) -> (Self, Receiver<VaultEvent>)`
  needs only the vault root, which `plugin_host` already reaches via
  `get_active_vault_manager().vault_path()`. Bridge its `mpsc` receiver â†’ `HookBus`
  in a spawned task. Feasible; lifecycle (when to start) is a prototype choice.

**Autonomy boundaries for a `/goal` run:**
- **Stop at end of Phase 4** (working + verified prototype). **Phase 5 is
  outward-facing** (posting to public issues/PRs) and stays a **manual review
  gate**, consistent with how we've handled every upstream artifact.
- **Live-vault assertion (Phase 4):** default to a **generic** check (search
  returns results with sane scores). A specific `query â†’ expected note` assertion
  needs a pair from the user; not required to proceed.
- **Environmental:** the live/real-embedding steps need `fastembed` to **download
  an ONNX model** (network + disk) on first run. Synthetic tests use a fake
  embedder (no download), so Phases 1â€“3 and most of 4 don't need it; the live test
  stays fail-closed if the model/vault is absent.

**Not blockers:** API shape churn (fork prototype â€” we control it; rework is the
point of dogfooding).

## Risks & mitigations

- **API shape churn** if Nick revises #42/#43 â†’ rework the plugin's wiring.
  *Mitigation:* keep capability code isolated and thin; the prototype's value is
  validating shapes, so churn is acceptable/expected.
- **Watcher reliability** across platforms (notify noise, rename semantics).
  *Mitigation:* prototype-level; the reconcile fallback guarantees correctness
  even if events are missed.
- **fastembed/usearch on Windows** (model download, native build).
  *Mitigation:* engine already builds + runs on Windows (Phase 0); live test is
  opt-in.
- **Multi-vault handle lifecycle** (per-vault index handles on active-vault
  switch). *Mitigation:* key engine handles by vault; mark rough edges
  `// prototype:`.
