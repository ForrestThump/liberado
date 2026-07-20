# TurboVault vault-events plugin — integration plan

**Status**: plan, 2026-07-18 (cold-start brief for implementers).  
**Primary vertical**: reimplement PR [#24](https://github.com/Epistates/turbovault/pull/24) subscription behavior as a **default-off compiled-in plugin** on the Phase 2 plugin boundary ([#39](https://github.com/Epistates/turbovault/pull/39)).  
**Umbrella**: [#34](https://github.com/Epistates/turbovault/issues/34) plugin & extensibility architecture.  
**Attribution contract**: [#33](https://github.com/Epistates/turbovault/issues/33) (partially satisfied by #39; vault-events closes the rest).  
**Liberado companion**: Decision 5 + `liberado-vault` / `VaultEventSource` (fallback already shipping).

This doc is the single place for another agent to understand **what to build, where it lives, what not to reinvent, and which forks need a deliberate choice.** It is not a line-by-line design; open the linked PRs/issues when you need code shape.

---

## 1. Cold-start orientation (read this first)

### What we are doing

Ship an optional TurboVault vertical so **MCP clients / models can subscribe to vault filesystem changes**, with:

- best-effort delivery (bounded queues, lag → resync, never a durable log of record);
- best-effort **who caused this?** attribution for loop prevention (never authz);
- tools namespaced under the plugin (`vault_events_*`), not core flat names;
- no raw `VaultManager` / MCP server handles in plugin code.

### What we are *not* doing

- Rebasing the old #24 core diff onto `main` (stale surface; Nick’s explicit instruction: **rebuild** on the plugin boundary).
- Putting subscriptions into the core 74-tool catalog.
- Treating provenance / Git author as security or multi-tenant isolation.
- Making Liberado *itself* a TurboVault plugin (different product decision; see §7).
- Dynamic JS / loadable plugins (Phase 4 of #34; deferred).

### One-sentence architecture

```
notify/watcher → vault-events plugin → HookBus envelopes (+ optional pull registry)
                                      → MCP tools vault_events_* for models
                                      → Liberado may keep in-process fallback or later consume bus/tools
```

### Guiding principle (#34)

> If it's a community plugin in Obsidian, it's a plugin in TurboVault.

Vault-event subscriptions are a vertical, like tasks and vector search—not core SDK.

---

## 2. Upstream landscape (truth table)

| Artifact | Role | Status (as of this plan) |
|---|---|---|
| [#28](https://github.com/Epistates/turbovault/issues/28) / provider split | Phase 1: core tools as composable providers | Done on modern main (foundation for plugins) |
| [#39](https://github.com/Epistates/turbovault/pull/39) | Phase 2: `turbovault-plugin-api`, host mount, `HookBus`, `VaultApi` | **Open, merge target** — implement against this (or merged main) |
| [#24](https://github.com/Epistates/turbovault/pull/24) | Original subscription registry + MCP pull tools | **Closed / not mergeable** — **behavioral reference only** |
| [#33](https://github.com/Epistates/turbovault/issues/33) | Provenance in event stream (A vs B, fail-open) | Open; **shared contract lives in #39**; FS/core-write correlation still open |
| [#34](https://github.com/Epistates/turbovault/issues/34) | Plugin umbrella + phased plan | Open; vault-events is Phase 3 item |
| [#19](https://github.com/Epistates/turbovault/issues/19) / tasks | Sister vertical (proving ground) | Parallel; same conventions |
| [#29](https://github.com/Epistates/turbovault/issues/29) / vector | Sister vertical | Parallel |
| Git write substrate (#37 / related) | Version tokens + history | Merged pieces exist; **static Git author ≠ per-agent provenance** |

### Nick’s Phase 3 path for #24 (authoritative)

After #39 lands:

1. Extract the vertical under `crates/plugins/`.
2. Feature: default-off `plugin-vault-events` (or similar) that **includes** `plugin-api`.
3. Translate watcher output into the **shared** `HookBus` / `VaultEventEnvelope` — do **not** invent a second event contract.
4. Bounded delivery; lag requires authoritative `VaultApi` resync.
5. Subscription tools only under `vault_events_*` namespace.
6. Provenance best-effort; `ExternalOrUnknown` **fail-open**, never auth.

### Nick on #33 after #39 ([comment](https://github.com/Epistates/turbovault/issues/33#issuecomment-5013573731))

- Shared contract is **implemented in #39**: `WriteProvenance`, content identity, `Attributed` vs `ExternalOrUnknown`, lag/resync.
- Plugin `VaultApi` writes already publish those envelopes.
- Attribution is **advisory loop-prevention**, never auth.
- **Does not close #33**: static Git author (vault/process) is history, not per-caller/session identity when many agents share one daemon.
- #39’s request context + hook boundary can carry that identity without exposing server internals.
- **Phase 3 vault-events** must translate **filesystem watcher** events into the same envelopes.
- **Core-write / audit / Git correlation for external edits** still needs an explicit best-effort design (that design is §5–§6 of this plan).

---

## 3. What #39 already gives you (do not re-specify)

Implementers should treat these as **stable contracts** once #39 is available. Depend only on `turbovault-plugin-api` from plugin crates.

### Crates / features

| Piece | Notes |
|---|---|
| `turbovault-plugin-api` | Publishable contract crate; plugins depend on this, **not** server internals |
| Host feature `plugin-api` | **Default-off**; vertical features must enable it + `dep:your-plugin` |
| Host `new_with_plugins` | Descriptor validation, namespace checks, CompositeHandler mount |
| Core tool catalog | Unchanged when no plugins registered (byte-for-byte flat names) |

### Plugin factory surface

```text
Plugin
  descriptor() → PluginDescriptor { id, name, version, description }
  build(PluginContext) → Arc<dyn PluginProvider>

PluginContext { vault: VaultApi, hooks: HookBus }

PluginProvider
  tools() → Vec<Tool>          // LOCAL names only (host prefixes)
  call_tool(name, args, PluginRequestContext) → ToolResult
```

- Plugin id must match `[a-z][a-z0-9_]*` (e.g. `vault_events`, `tasks`).
- Host advertises tools as `<id>_<local>` → `vault_events_subscribe`, not bare `subscribe`.
- `PluginRequestContext`: request_id, optional user/session/client ids, serializable metadata — **no** raw transport sessions, principals, headers, managers.

### VaultApi (CAS-only facade)

| Op | Contract |
|---|---|
| `active_vault` | name + write_backend label |
| `list_notes` | markdown paths |
| `read_note` | full content + opaque **version** token |
| `write_note` | **must** use `CreateOnly` or `Match(version)` — **no blind overwrite** |

Version tokens are backend-native (content hash on legacy, Git blob id on git). Treat them as opaque.

Optional `WriteProvenance` on write requests feeds hook attribution for **plugin-path writes**.

### HookBus

| Type | Meaning |
|---|---|
| `VaultEventEnvelope` | `sequence`, `observed_at_ms`, `vault`, `event`, `content_hash?`, `attribution` |
| `HookEvent` | `FileCreated/Modified/Deleted/Renamed` **or** `ResyncRequired` |
| `EventAttribution` | `Attributed(WriteProvenance)` \| `ExternalOrUnknown` |
| `HookRecvError::Lagged { skipped }` | Subscriber fell behind → **must resync via VaultApi**, not invent missing events |
| `Closed` | Bus shut down after draining buffers |

Delivery is **best-effort broadcast** with a fixed per-subscriber ring. Success of `publish` does not imply a live consumer.

### What #39 does *not* do (vault-events fills this)

- Does **not** run a filesystem watcher for all external edits.
- Does **not** expose `subscribe` / `fetch` MCP tools.
- Does **not** implement per-client glob filters / long-poll pull registry (#24 had those).
- Host publishes to `HookBus` primarily for **writes through `VaultApi`**. Watcher → bus is the plugin’s job (and optionally host evolution later).

---

## 4. What #24 contributes as a *reference* (rebuild, don’t port blindly)

Study the PR / branch for **behavior and tests**, not structure.

High-signal behaviors to preserve:

| Behavior | Why it matters |
|---|---|
| One watcher, many logical subscribers | OS watcher handles are expensive; don’t N-watch the same path |
| Bounded per-subscriber queues | Slow clients must not OOM the server |
| Drop / lag → client resyncs from vault | Explicit at-most-once; no pretend durability |
| Glob + kind filters | Models care about zones (`inbox/**`, kinds=created/modified) |
| Pull long-poll + sequence / `since_seq` | Works over MCP without push notifications; resume-friendly |
| Idle reaper | Crashed clients must not leak subscriptions forever |
| In-memory only | Restart ⇒ re-subscribe; document it |

**Do not** put a second parallel event type system next to `HookBus`. Either:

- **Fork A (preferred by Nick):** plugin translates watcher → `HookBus`; pull tools drain a **plugin-local** filtered view of the bus (or of a thin registry fed by the bus); or  
- **Fork B:** plugin owns a private registry only (like #24) and *also* publishes a summary into `HookBus` for in-process plugins — risk of dual pipelines; only if pull semantics cannot map cleanly onto broadcast.

Default recommendation: **Fork A** — one producer path into `HookBus`, filters/cursors as consumer-side state inside the vault-events provider.

### Known upstream watcher bug (do not paper over forever)

`VaultEvent::FileRenamed` is often **dead** in practice: `notify` rename kinds collapse into `FileModified`. Cross-platform fix needs a correlation buffer (Linux Both vs macOS/Windows From/To).  

**Fork:**

| Choice | Guidance |
|---|---|
| **Split rename fix into its own PR** | Preferred: reviewable, benefits all consumers |
| **Ship vault-events treating rename as Modified** | Acceptable v0 if documented; clients must not rely on FileRenamed |

Liberado’s daemon already largely ignores deletes and doesn’t depend on rename fidelity—same caveat.

---

## 5. Target design: `vault_events` plugin

### Placement (convention)

```text
turbovault/
  crates/
    turbovault-plugin-api/     # host contract (#39)
    plugins/
      turbovault-plugin-vault-events/   # THIS vertical
    turbovault/                # binary host; feature-wires the plugin
```

Exact crate name can match tasks/vector siblings; keep `plugin-` prefix consistent with whatever tasks uses when it lands.

### Feature wiring (pattern for *all* verticals)

```toml
# crates/turbovault/Cargo.toml (illustrative)
[features]
plugin-api = ["dep:turbovault-plugin-api", ...]
plugin-vault-events = ["plugin-api", "dep:turbovault-plugin-vault-events"]
```

Default binary: **no** plugin features. Homelab / Liberado images enable explicitly.

### Descriptor sketch

| Field | Value |
|---|---|
| `id` | `vault_events` |
| local tools (examples) | `subscribe`, `fetch`, `unsubscribe`, maybe `status` |
| advertised | `vault_events_subscribe`, `vault_events_fetch`, … |

Tool schemas should mirror #24’s intent (filter globs/kinds, timeout, max events, handle, since_seq) but bind against the new provider facade and envelope types.

### Runtime responsibilities

1. **Produce events**  
   - Start / stop watcher for the **active vault** (lifecycle must track vault switch — #24 pinned first vault; modern host is active-vault aware; **do not regress**).  
   - Map watcher events → `HookEvent` + best-effort `content_hash` / `attribution`.  
   - `hooks.publish(...)`.  
   - On producer reset / unrecoverable lag internal state → `HookEvent::ResyncRequired`.

2. **Serve models (MCP pull)**  
   - Maintain optional per-session subscription state (filter, cursor, queue).  
   - `subscribe` → handle; `fetch` long-poll → envelopes + next cursor + dropped/lag signal; `unsubscribe` / idle reaper.  
   - On lag: surface explicitly so the model resyncs (list/read via core tools or `VaultApi` if co-process).

3. **Stay behind the facade**  
   - Prefer `VaultApi` for reads used in attribution.  
   - If the host must expose a watcher capability later, that is a **host PR**, not a plugin reaching into `VaultManager` privately (violates #39 safety story). If v1 truly cannot watch without a host extension, land a minimal **curated** host hook (e.g. “give plugin a watcher factory”) rather than leaking managers—document that as a dependency of the vertical.

> **Implementation fork (host capability):**  
> - **H1:** Host adds a small, documented “filesystem observation” capability to `PluginContext` (watcher stream or callback). Cleanest long-term.  
> - **H2:** Plugin uses only public crates already allowed (if any) without managers — only if still true after #39; verify before assuming.  
> - **H3:** Temporary feature-gated host glue inside `turbovault` that constructs the provider with a watcher handle (not ideal, but unblocks Phase 3). Prefer graduating H3 → H1.

### Attribution design for envelopes (closes the open half of #33)

Three event origins, one envelope type:

| Origin | How attribution is set | Signal quality |
|---|---|---|
| **Plugin `VaultApi` write** | Host already sets `Attributed` from request provenance (#39) | High for that write path |
| **Core MCP write tools** (`write_note`, etc.) | **Not automatic today.** Need best-effort correlation (audit metadata, write-path publish, or content join) | Medium if wired; else ExternalOrUnknown |
| **External FS** (Obsidian, git, sync) | Content-identity join vs audit / recent write cache; else `ExternalOrUnknown` | Fail-open by design |

#### Recommended correlation policy (best-effort, explicit)

Adopt Liberado’s proven **Approach A** semantics as the default for watcher-origin events:

```text
on FileModified(path):
  content = read via VaultApi (if missing → FileDeleted path or ExternalOrUnknown)
  hash = backend-native content identity (same function the host uses for version/after_hash)
  if recent write record exists with after_hash/version == hash:
      attribution = Attributed(that write's provenance)
  else:
      attribution = ExternalOrUnknown   # FAIL OPEN → consumers may react
```

**Hard rules (non-negotiable):**

1. Attribution is **never** an auth boundary.  
2. Missing / ambiguous / lag → treat as external or `ResyncRequired`, never “trusted suppress.”  
3. **No frontmatter provenance** (false suppress on human edit; pollutes the event stream).  
4. Match on **content identity**, not “latest write to path by time” alone (handles coalesce + human-after-agent).  
5. Move/rename: attribute against the **resulting** path’s content; do not suppress a later recreation of the old path because a move entry still mentions it (Liberado regression; keep it).

#### Approach A vs B (from #33) — how it maps now

| Approach | Meaning | Status after #39 |
|---|---|---|
| **A consumer-side join** | Client reads audit + hashes | Still valid for out-of-process Liberado fallback; no dependency on vault-events |
| **B server-side enrichment** | Envelope carries attribution | **#39 envelope fields = B’s shape**; vault-events fills B for **watcher** events |
| **Git author only** | Static process author | History only; **does not** identify multi-agent callers (#33 comment) |

**Recommendation:** implement **B-shaped envelopes** for everything the plugin publishes (watcher + any enrichment), using **A’s join algorithm** inside the producer. Consumers (models, Liberado) prefer reading the envelope; they may still re-join for defense in depth.

#### Multi-agent identity (#33 remaining gap)

When several agents share one TurboVault daemon:

- Prefer provenance `source` + `correlation_id` from the **write path** (plugin write or core tool `_meta` / audit metadata once wired).  
- Use `PluginRequestContext` / session fields for **tool-call identity**, not for FS events (FS has no session).  
- Git author = optional history breadcrumb, not the loop-break key.

Closing full core-tool → envelope correlation may require a small host change (publish on core writes, or expose audit query through a curated API). Track that as a **sub-deliverable of #33**, not a blocker for a first vault-events MVP that only attributes what it can and fail-opens otherwise.

---

## 6. Liberado integration (companion system)

### What Liberado already has (do not rewrite)

| Component | Role |
|---|---|
| `liberado-common::WriteProvenance` | Nearly identical to plugin-api; **extra field** `zone` |
| `liberado-vault` | Provenance-tagged writes via audit metadata; hash-join `attribute` / `should_react` |
| `Vault.watch` + `VaultEventSource` | Documented **§8.1 fallback** for unmerged #24 |
| Debounce + standardized `Event` | Daemon mesh; `EventSource` trait (vault + cron) |
| Decision 5 / concurrency spec | Loop-break, idempotency, fail-open |

Code anchors: `crates/vault/`, `crates/daemon/src/vault_source.rs`, `docs/specs/liberado-vault-concurrency-spec.md`.

### Dual-plugin stories (do not confuse)

| Layer | “Plugin” means | Status |
|---|---|---|
| **TurboVault** | Compiled-in MCP vertical in the TV binary | **This plan** |
| **Liberado mesh** | Vault is default perception plugin behind `EventSource` | **Already done** (Decision 19) |

### Liberado adoption forks (choose deliberately later)

| Option | When | Tradeoff |
|---|---|---|
| **L0 — Keep fallback forever for daemon** | Always valid | Two watchers if TV also watches; simplest ops; Liberado works without TV plugin feature |
| **L1 — Out-of-process MCP client** | Daemon talks HTTP to TV `vault_events_*` | Single watcher in TV; network lag; needs TV feature on; good for multi-process homelab |
| **L2 — Co-process HookBus** | Liberado linked into same process as TV (unusual today) | Lowest latency; couples deployables |
| **L3 — Liberado vertical *as* TV plugin** | Want Liberado tools inside TV binary | Different roadmap; not required for event subscribe |

**Default path:** ship vault-events for **models / MCP clients** first; leave Liberado on **L0** until L1 is clearly better. Document that Liberado’s fallback remains the authoritative loop-break implementation until L1 proves parity.

### Doc drift to fix when shipping

- Specs saying “consume PR #24 natively” → “consume `vault_events` plugin / HookBus; rebuild of #24 on #39.”  
- `liberado-vault` comments citing “not-yet-merged PR #24” → point at this plan + plugin feature flag.

### Provenance field alignment

| Liberado | plugin-api |
|---|---|
| `source`, `correlation_id`, `note`, **`zone`** | `source`, `correlation_id`, `note` |

Map `zone` into `note` or request metadata when crossing the TV boundary, or propose an additive optional field upstream if multi-plugin consumers need it. Do not block on `zone`.

---

## 7. How to write TurboVault plugins (general)

Use this section for **any** new vertical (tasks, vector, future Liberado-adjacent tools).

### Rules of the road

1. **Compiled-in, feature-gated, PR-reviewed.** No dynamic load, no FFI, no marketplace install in v1.  
2. **Depend on `turbovault-plugin-api` only** for host capabilities. Broader plugin-owned Rust APIs for non-MCP callers are allowed but reviewed as normal public API surface (documented in #39).  
3. **Local tool names only**; host namespaces with `id`.  
4. **Vertical feature defaults off** and must enable `plugin-api`.  
5. **CAS writes only** through `VaultApi` — no blind overwrite escape hatch.  
6. **Hooks are advisory** — lag, close, ExternalOrUnknown are first-class; never invent reliability the bus doesn’t have.  
7. **Core tools stay flat and stable** when your feature is off.  
8. **Helpers shared across plugins** wait until 2–3 plugins show real overlap (#34).

### Minimal skeleton

1. Crate under `crates/plugins/…` with `Plugin` + `PluginProvider` impls.  
2. Descriptor with stable `id`.  
3. `build`: store `VaultApi` + `HookBus` in the provider.  
4. `tools` / `call_tool`: pure local names; validate args; map errors to `PluginError` codes.  
5. Host: feature flag + register factory in `new_with_plugins` list.  
6. Tests:  
   - feature-off: core tool list unchanged;  
   - feature-on: namespaced tools appear;  
   - namespace / duplicate rejection;  
   - write precondition conflict;  
   - hook lag/resync if you use the bus;  
   - no access to forbidden internals (review-level).

### What belongs in a plugin vs core

| Put in **plugin** | Keep in **core** |
|---|---|
| Opinionated verticals (tasks, events, vector, calendars, …) | Read/write/graph/search/health primitives |
| Namespaced multi-tool workflows | Flat stable tool names used by all clients |
| Extra indexes, watchers, domain schemas | Shared CAS / multi-vault / security boundary |

### Safety checklist (every PR)

- [ ] Blind overwrite impossible via plugin API  
- [ ] No raw manager/server/session leakage  
- [ ] Duplicate plugin id / tool rejected at boot  
- [ ] Attribution / hooks fail open  
- [ ] Default features do not pull the vertical  
- [ ] Docs: enable instructions + non-goals  

---

## 8. Work plan (phases for vault-events)

### Phase 0 — Prerequisites

- [ ] Merge or track `feat/plugin-api-phase-2` (#39) on the fork used by Liberado/homelab.  
- [ ] Confirm local `turbovault/` has `turbovault-plugin-api` and host mount path.  
- [ ] Read #39 `docs/development/plugins.md` + this plan + #24 behavior notes.  
- [ ] Decide H1/H2/H3 for watcher capability (§5).

### Phase 1 — Skeleton (no real watcher yet)

- [ ] Scaffold `turbovault-plugin-vault-events`.  
- [ ] Wire `plugin-vault-events` feature.  
- [ ] Empty or stub tools; boot tests for namespace + feature-off parity.  
- [ ] Subscribe to `HookBus` in-process and assert host plugin-write envelopes are visible (proves context plumbing).

### Phase 2 — Watcher → HookBus

- [ ] Active-vault-aware observation pipeline.  
- [ ] Publish `HookEvent`s with fail-open attribution (MVP may start as always `ExternalOrUnknown` for FS).  
- [ ] `ResyncRequired` on internal failure.  
- [ ] Tests: create/modify/delete (and rename if fixed).  
- [ ] Document rename limitation if not fixed yet.

### Phase 3 — Model-facing pull tools (reimplement #24 UX)

- [ ] Subscription registry **inside** the plugin (filters, long-poll fetch, reaper, drop counters).  
- [ ] Tools: subscribe / fetch / unsubscribe (+ status if useful).  
- [ ] Lag / dropped surfaced in fetch results.  
- [ ] Stdio or HTTP MCP e2e: model-shaped client can long-poll changes.

### Phase 4 — Attribution quality (#33 remainder)

- [ ] Content-identity join for watcher events (Approach A algorithm, B-shaped envelopes).  
- [ ] Correlation for core MCP writes (host publish and/or audit metadata join).  
- [ ] Multi-agent provenance via write path + request context; document Git author as non-sufficient.  
- [ ] Adversarial cases: human-after-agent, move then recreate source, coalesce bursts, lag mid-join.

### Phase 5 — Liberado optional consume

- [ ] Keep L0 fallback green.  
- [ ] Optional L1 adapter behind a Liberado config flag (HTTP subscribe/fetch).  
- [ ] Parity tests: same external edit produces one reaction under L0 and L1.  
- [ ] Update Decision 5 / architecture docs.

### Phase 6 — Upstream hygiene

- [ ] Separate rename-correlation PR if not done.  
- [ ] PR to Epistates with default-off feature, tests, plugins.md update.  
- [ ] Homelab image: opt-in feature flag for TV container.  
- [ ] Do not force Liberado binary change on TV default builds.

---

## 9. Tradeoffs & decision log (forks)

Use this table when a subagent or human hits an ambiguity—**pick explicitly**, don’t half-do both.

| ID | Fork | Prefer | Avoid |
|---|---|---|---|
| F1 | Rebuild #24 vs rebase | Rebuild on #39 | Porting monolithic tools.rs patches |
| F2 | Event contract | Single `HookBus` envelope | Parallel EventEnvelope types |
| F3 | Pull registry location | Plugin-local on top of bus | Core subscription module |
| F4 | Attribution | Fail-open ExternalOrUnknown | Silent suppress without hash match |
| F5 | Provenance storage | Audit / write-path / envelope | Frontmatter keys |
| F6 | Git author | Optional history | Multi-agent identity |
| F7 | Watcher host access | Curated capability (H1) | Leaking VaultManager into plugins |
| F8 | Rename correctness | Separate PR | Shipping FileRenamed that never fires without docs |
| F9 | Liberado consume | L0 first; L1 later | Blocking vault-events on Liberado rewrite |
| F10 | MVP scope | Watcher→bus + minimal tools | Perfect core-write correlation on day one |
| F11 | Queue model | Bounded + lag signal | Unbounded channels |
| F12 | Durability | At-most-once + resync | Pretending the bus is a journal |

---

## 10. Testing strategy (high signal)

| Layer | What to prove |
|---|---|
| Unit | Filter matching, seq gaps, reaper TTL, attribution join edge cases |
| HookBus | Lag forces resync path; close drains then errors |
| Provider | Namespace prefix; unknown tool; invalid filter limits |
| Feature matrix | Default build = no vault_events tools; all-features builds clean |
| MCP e2e | subscribe → external file write → fetch sees envelope |
| Loop-break | Agent write with provenance → attributed → consumer suppresses; raw Obsidian edit → ExternalOrUnknown → react |
| Regression | Move then human recreate source path must **not** suppress |
| Liberado (later) | L0 vs L1 parity on a temp vault |

Mutation / property ideas (optional): drop counter monotonicity; never attribute without hash match.

---

## 11. Success criteria

**MVP (Phase 1–3):**

- Default-off feature; core catalog unchanged when off.  
- With feature on, models can subscribe/fetch vault changes via `vault_events_*`.  
- Envelopes use shared types from `turbovault-plugin-api`.  
- Lag/resync documented and tested.  
- No manager/server leakage in the plugin crate.

**Done for #33 (Phase 4):**

- Watcher-origin events can be attributed when content matches a known write; otherwise ExternalOrUnknown.  
- Multi-agent identity story documented (write provenance + request context, not static Git author).  
- Explicit statement: attribution is not authz.

**Done for Liberado (Phase 5, optional):**

- Fallback still works.  
- Optional path to consume TV plugin without dual semantics drift.

---

## 12. Key references (open these when coding)

### Upstream

- PR [#39](https://github.com/Epistates/turbovault/pull/39) — plugin API + host (implementation target).  
- PR [#24](https://github.com/Epistates/turbovault/pull/24) — subscription behavior reference (closed).  
- Issue [#33](https://github.com/Epistates/turbovault/issues/33) — provenance discussion; [Nick’s status comment](https://github.com/Epistates/turbovault/issues/33#issuecomment-5013573731).  
- Issue [#34](https://github.com/Epistates/turbovault/issues/34) — umbrella phases & conventions.  
- #39 doc: `docs/development/plugins.md` (once branch is checked out).

### Liberado

- `docs/specs/liberado-vault-concurrency-spec.md` — Decision 5, Approach A, zones, idempotency.  
- `docs/specs/liberado-architecture-decisions.md` — Decisions 5, 6, 18, 19.  
- `docs/specs/life-os-architecture.md` §5 — triggering layer (update when plugin lands).  
- `crates/vault/` — attribution + write adapter.  
- `crates/daemon/src/vault_source.rs` — production EventSource fallback.

### Local repo notes

- Sibling `turbovault/` path dep may lag upstream; do not assume `plugin-api` exists until Phase 0.  
- Homelab currently builds TV from fork branches (`develop` etc.) — feature flags must be set in that Dockerfile/build when enabling the vertical.

---

## 13. Suggested first PR sequence (for the implementing agent)

1. **Upstream (or fork):** merge/track #39.  
2. **Skeleton PR:** empty `vault_events` plugin + feature flag + feature-off parity tests.  
3. **Watcher→bus PR:** produce envelopes; MVP attribution ExternalOrUnknown.  
4. **Tools PR:** pull subscribe/fetch/unsubscribe + e2e.  
5. **Attribution PR:** content join + core-write correlation design (may split host changes).  
6. **Rename PR:** independent watcher correctness.  
7. **Liberado docs/adapter PR:** optional; only after 3–4 are stable.

Each PR should stay reviewable; do not bundle rename correlation + pull registry + Liberado L1 in one change.

---

## 14. Glossary

| Term | Meaning |
|---|---|
| **HookBus** | Process-local bounded broadcast of `VaultEventEnvelope` |
| **VaultApi** | Curated CAS read/write facade for plugins |
| **Attribution** | Best-effort “who wrote these bytes” for loop prevention |
| **Fail-open** | Unknown provenance ⇒ treat as external (allow react) |
| **Lag** | Subscriber missed events; must resync authoritative state |
| **Vertical** | Feature-gated opinionated plugin, not core SDK |
| **L0 / L1** | Liberado local watcher vs MCP consume of vault-events |

---

*End of plan. If this conflicts with a newer maintainer comment on #33/#34/#39, prefer the maintainer comment and update this file.*
