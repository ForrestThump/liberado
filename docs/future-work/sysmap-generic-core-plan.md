---
kind: plan
status: active
authority: implementation
domain: tooling
open_items: true
---

# sysmap — split into a generic core + a Liberado profile

**Status**: active, 2026-08-15. **Phase 1 done** — `sysmap-core` extracted (pure move) and its
layer/kind vocabulary opened (`Layer`/`NodeKind` are string ids against a `Vocabulary` carried on
`SystemMap`; the Liberado palette lives in `liberado-sysmap/src/profile.rs`). **Phases 2–6 open.**
This plan records the split of the isometric/3D system map (`liberado-sysmap` +
`liberado-sysmap-gui`, see [`crates/sysmap/README.md`](../../crates/sysmap/README.md)) into a
**project-agnostic** core crate plus a thin **Liberado-specific** profile, so the map becomes
portable to other Rust projects.

## The principle: three sources, three homes

1. **What cargo can prove** (crates, dependency edges) → derive at runtime with `cargo metadata`.
   No hardcoding, fully portable.
2. **What a human or LLM must assert** (roles, runtime flows, colors, explainer prose, runtime
   instances) → a `sysmap.toml` template the generic core reads.
3. **What is project-specific *code*** (parsing *this* project's own config schema) → a thin
   per-project adapter. For Liberado that adapter is the `topology.toml` reader (~150 lines); for a
   project with no runtime config it is zero lines.

Today `liberado-sysmap` mixes all three. The plan separates them into three crates:

* **`sysmap-core`** — generic, liftable, publishable. Cargo derivation + layout + projection +
  the `sysmap.toml` rule engine. **No `liberado-` dependency.**
* **`liberado-sysmap`** — the Liberado adapter: reads `topology.toml`, emits extra nodes/edges and
  supplies the profile (layers, kinds, colors, edge rules).
* **`liberado-sysmap-gui`** — the renderer. Already generic (it only touches the model); it should
  compile against `sysmap-core` alone as the proof of decoupling.

## Where every current hardcode goes

| Today (hardwired in `liberado-sysmap`) | New home |
|---|---|
| `is_internal()` = `liberado-` / `chat-client-contract` prefix | **Derived**: dependency resolves to a workspace member |
| `crates/*/Cargo.toml` scan path | **Derived**: workspace members from `cargo metadata` (any layout) |
| `repository_root()` walk-up heuristic | **Derived**: `cargo metadata`'s `workspace_root` |
| `Layer` enum (10 roles) + `blurb()` | `sysmap.toml` `[[layers]]` (id, label, color, blurb, order) |
| `layer_color()` palette | `[[layers]].color` |
| `NodeKind` enum + `label()` | `sysmap.toml` `[[kinds]]` |
| `kind_color()` palette | `[[kinds]].color` |
| `MAIN_STACK` ordering | `[[layers]]` order |
| Seed edge `vault → liberado-daemon` | `sysmap.toml` `[[edges]]` |
| The `topology_edges()` rules | `sysmap.toml` `[[edge_rules]]` + `[[routes]]` |
| `mcp_writes_vault`, `profile_domain_is_coding` | Adapter emits `meta.writes_vault` / `meta.domain`; the *rule* is generic |
| `topology.toml` → node mapping | Adapter (uses `liberado-config-loader`) — the one genuinely per-project code |
| Edge/scene colors, explainer text | `sysmap.toml` (with hash-based defaults in core, so zero-TOML still renders) |

The one thing that **never becomes derivable** is semantic *meaning* — the `"decision → Task +
provenance"` labels, the layer blurbs, the judgment that "MCP X writes the vault." Cargo and even
`rustdoc --output-format json` can suggest *call* relationships, but not *control vs data* or *why*.
That is exactly what the template is for.

## The `sysmap.toml` template

```toml
[project]
name = "Liberado"
manifest_namespace = "liberado"   # reads [package.metadata.<ns>] role + flows

# Crate grouping, bottom-to-top. `main = true` layers stack in the main district.
[[layers]]
id = "kernel"
label = "Kernel"
color = "#4f7ce0"
blurb = "The orchestration engine: decide/act loops, sessions, capability."
main = true
# … repeat per layer; main = false puts a layer in the side "meta" district

# Non-crate node kinds.
[[kinds]]
id = "mcp"
label = "MCP server"
color = "#6a8fd0"
blurb = "Out-of-process tool server"
height = 0.95

# Extra nodes not in cargo (the adapter may also emit these programmatically).
[[nodes]]
id = "vault"
label = "vault"
kind = "vault"
layer = "store"
description = "The Obsidian vault — source of truth"

# Static runtime edges.
[[edges]]
from = "vault"
to = "liberado-daemon"
kind = "data"
label = "external change"

# Rule: apply to every matching node.
[[edge_rules]]
when = "kind=mcp"                       # selector: kind / layer / id glob
if_meta = { writes_vault = "true" }     # optional predicate
to = "vault"
kind = "data"
label = "zone write"
dir = "out"                             # matched node → to (or "in" for to → node)

# Value-dependent routing (profiles → domain packs).
[[routes]]
when = "kind=profile"
to = "liberado-dispatch-pack"           # default
kind = "control"
label = "domain pack"

[[routes]]
when = "kind=profile"
if_meta = { domain = "coding" }
to = "liberado-coder-agent"
kind = "control"
label = "domain pack"
```

`manifest_namespace` is the decoupling trick: the core reads `[package.metadata.<ns>] role` and
`flows` with `<ns>` defaulting to `sysmap`. Liberado passes `liberado`, so **zero manifest churn** —
and `layer_rules` and the crate-map generator keep reading `[package.metadata.liberado] role`
untouched (see [`../spec/architecture/contracts.md`](../spec/architecture/contracts.md)).

## What `cargo metadata` buys over the current toml parse

* Internal-dep detection = *workspace membership*, not a name prefix (the real portability win).
* Dep *kinds*: dev/build/target-specific, with an include/exclude switch instead of silently
  dropping them.
* Renames (`dep = { package = "foo", … }`) — today the parser uses the TOML key, not the resolved
  name.
* Workspace members in any location (no `crates/` assumption), features/optional deps, editions,
  versions.
* `workspace_root` discovery (replaces the walk-up).

Cost, stated honestly: the core starts shelling out to `cargo` (`cargo_metadata` crate, ~100–300
ms), and `scan.rs`'s test fixtures — currently fake manifests with non-resolving `workspace = true`
deps — must become **valid cargo workspaces**. That is the one real migration cost; everything else
is relocation.

## Why not derive semantics from rustdoc

`rustdoc --output-format json` is nightly-only, its format is explicitly unstable, and it is
designed for public-API analysis, not call-graph extraction. Its `links` field conflates type use,
trait impls, cross-crate calls, and doc mentions, and omits intra-crate body references — so the
daemon's own loop, the thing the runtime edges describe, is invisible to it. It can at most
**suggest candidate** runtime edges ("A references B's `send_task`") for a human/LLM to confirm and
label into the template. That "derive candidates → annotate" mode is a possible *later* phase, never
a source of truth. The only accurate semantic source is dynamic: `tracing` spans on the running
daemon, which is a separate future "live mode", not static analysis.

## Phases

**Phase 1 — extract `sysmap-core` (pure move, no behavior change).**
New crate `crates/sysmap-core/`, `role = "tooling"`, **no `liberado-` dependency**. Move
`model.rs`, `layout.rs`, `iso.rs`, `style.rs`. Make `Layer`/`NodeKind` **open**: `String` ids
resolved against the profile (an undeclared layer gets a hashed fallback color). `liberado-sysmap`
becomes a wrapper re-exporting the same `build()` / `SystemMap` API so the GUI and tests don't
break. Gate: build + tests green before any further change.

**Phase 2 — swap the scanner to `cargo metadata`.**
Replace `scan_repository`/`read_manifest` with `cargo_metadata`. Internal = workspace-member
resolution. This deletes `is_internal` and the `crates/` assumption. Confirm the exact flags that
avoid registry resolution (`--no-deps`) here. New test fixtures are valid temp workspaces.

**Phase 3 — move declared content into `sysmap.toml`.**
Layers, kinds, colors, blurbs, the seed edge, the `topology_edges` rules, and profile routing become
data. Core gains the small rule engine (selectors over `kind`/`layer`/`id` + `meta` predicates).
`manifest_namespace = "liberado"` keeps existing manifests working.

**Phase 4 — shrink the shim to a topology adapter.**
`liberado-sysmap` keeps only: read `topology.toml` via `liberado-config-loader`, emit
`Vec<MapNode>` + computed `meta` flags (`writes_vault`, `domain`), call
`sysmap-core::build(root, profile, extra_nodes)`. Roughly 150 lines.

**Phase 5 — prove the decoupling.**
`liberado-sysmap-gui` must compile against `sysmap-core` alone. If it does, the generic crate is
genuinely liftable. The legend reads layer/kinds from the profile, so colors cannot drift from the
data.

**Phase 6 — publish / port.**
`sysmap-core` becomes `sysmap` (name TBD) on crates.io or a git dep. The portability contract is the
`sysmap.toml` schema + "adapter = parse your config → nodes". A new Rust project needs the crate +
one `sysmap.toml` (+ an adapter only if it has runtime config) to get the same map.

## What stays green, and the one rule change

`layer_rules`, the crate-map generator, clippy, fmt, and `--locked` metadata all stay green — `role`
never moves. The only new layer-rules obligation is giving `sysmap-core` a `role = "tooling"` in its
own manifest, which is already the pattern. The pre-existing `liberado-sysmap` tests move wholesale
into `sysmap-core`; they assert behavior, not the toml-parsing internals, so most assert-the-same
and only the fixture setup helper changes (Phase 2).
