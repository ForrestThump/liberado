# Liberado Config Spec — One Resolved Model, Many Small Files, Fail-Fast

**Status**: Resolves Tier-3 Decision 14 (single source of truth for config / topology). Actionable.
**Owner**: Shiloh Mangus
**Last Updated**: August 29, 2026
**Related**:
- `liberado-architecture-decisions.md` (Decision 14; Decision 10 secrets; Decision 4 policy)
- Every companion spec contributes tunables (their "Tunables — single source of truth" tables)

---

## 1. Principles

1. **Single source of truth = one resolved, validated *model* — not one file.** Many small files are
   loaded and merged into one typed config object at daemon startup. Each individual *setting* is
   owned by exactly one place (the validator rejects duplicate ownership), so single-source-of-truth
   holds at the granularity that matters, without a monster file.
2. **Defaults live in code; config holds only the deltas.** Every tunable has a `Default` impl set to
   the value specced in its home document. The config file contains only what the user wants to
   *change* — a fresh install runs on an **empty or absent** config and still works.
3. **Fail-fast.** The merged config is validated *before* the daemon serves anything. Conflicts are a
   **load-time error, not a runtime surprise**.
4. **Out of the vault, homelab-local.** Config lives on the machine running the system; you `ssh` in
   to change it. It is never synced into the vault (boot-order chicken-and-egg; and a sync conflict
   or phone edit must never be able to alter the containment boundary).
5. **Agents do not write config.** No MCP, hook, dispatcher, or subagent has a capability to modify
   config files. (User-approval-gated config changes *through* the system are a v2+ roadmap item,
   explicitly out of scope for initial work.)
6. **Secrets are not config.** They live in env / systemd credentials and are referenced by name
   (Decision 10); raw secret values never appear in any config file.

## 2. The Three Kinds of Config

Config is three different concerns wearing one name; they have different owners, change cadences,
and risk profiles:

| Kind | Examples | Why it lives where it does |
|---|---|---|
| **Topology / wiring** | enabled MCPs/hooks, ports, socket paths, webhook URLs, model/provider selection | The daemon needs the whole picture to wire and boot. Homelab-local. |
| **Security policy** | zones, write-classes, capability grants, secret *references* | The containment surface (Decision 4). **Must be central + auditable in one place** — never scattered per-module, or a module could under-declare its own limits. |
| **Behavior tunables** | thresholds, settle windows, `MAX_*`, schedules | Benign (a wrong value degrades behavior, never breaches containment). Mostly defaults-in-code with optional overrides. |

## 3. File Layout & Precedence

Split by **concern**, not per-module (avoids file proliferation):

```
/etc/liberado/            (or ~/.config/liberado/ — XDG)
├── topology.toml         # wiring: components + enablement, transports, ports/sockets, models
├── policy.toml           # security surface: zones, write-classes, capability grants, secret refs
└── tuning.toml           # OPTIONAL overrides of code defaults; usually small or absent
```

- **Format**: TOML (Rust-idiomatic, unambiguous typing — avoids YAML's coercion surprises). Not in
  the vault, so consistency with Obsidian's YAML frontmatter is not a concern.
- **Table scope is positional.** A header such as `[main_agent]` owns every key after it until the
  next table header. Keep top-level keys such as `vault_path`, `daemon_socket`, and `provider`
  before the first table. This is wrong:

  ```toml
  [main_agent]
  delegation_mode = true
  provider = "openrouter" # main_agent.provider, not the global provider
  ```

  Liberado rejects unknown keys in `[main_agent]` so this mistake fails during config load instead
  of silently leaving the global provider at its `deepseek` default.
- **Merge precedence (lowest → highest):**
  1. **Code defaults** (the `Default` impls).
  2. **Config files** (`topology` / `policy` / `tuning`).
  3. **Environment variables** (`LIBERADO_*`).
  4. **CLI flags** (highest; for one-off overrides).
- Each setting resolves to the highest-precedence source that provides it; everything else falls back
  to the code default. This is why the files can be tiny.

## 4. Validation (the fail-fast contract)

A single loader merges all sources into the typed model, then runs **cross-cutting validation**
before anything starts. Examples of what it rejects:

- A capability or write-class that references a **zone not defined** in `policy`.
- A hook/MCP named in `policy` (grants) that **does not exist / is not enabled** in `topology`.
- **Port / socket-path collisions** across components.
- An enabled hook with **no trigger** (neither subscription routing nor a webhook/timer).
- A **secret reference** with no corresponding env/systemd credential present.
- **Duplicate ownership** of a setting across files.
- Unknown keys in `[main_agent]`, including top-level keys placed below that table header.
- Out-of-range tunables (e.g. `MAX_CONCURRENT_SUBAGENTS = 0`).

Surfaced two ways:
- **On daemon startup** — refuses to start, prints actionable errors.
- **`liberado config check`** — validates the merged config without starting the daemon (CI-able;
  run after any `ssh` edit before restarting).

## 4.1 Machine-owned overlays and install secrets

Two files live outside the three section files and are written by the running system, never by hand:

- **Grants overlay** (`<data_dir>/grants.overlay.toml`). When a human taps
  "Approve everywhere", the daemon appends the grant here - never into the
  hand-edited `policy.toml`. At boot the overlay is parsed and its
  grants/zones APPENDED to the base policy (base entries keep priority, so
  the overlay can only add authority for a zone the base never declared),
  and the merged candidate is validated like any policy. It is deliberately
  soft: a missing overlay is the common case; a broken one is logged and
  ignored rather than blocking boot. Deleting the file revokes every such
  grant.
- **Proposal signing key** (`<data_dir>/.proposal-key`, 32 random bytes,
  generated on first use). Runtime downgrade proposals are signed with it
  for integrity. If it cannot be persisted, the run falls back to an
  ephemeral key and warns - proposals created then simply fail verification
  after a restart, which is the safe direction (rejected, not accepted).

## 4.2 Pack-section arrival is a composition contract

A value that parses is not a value the runtime uses. `[coder]` rides through
`liberado-config` as an opaque `toml::Value`. The coding pack assembles it with
`liberado_coder_core::CoderTuning::from_value` and `run_config()`. Literal
construction rules catch a surface that hard-builds `CoderRunConfig`. They
cannot catch a setting that defaults and is never the operator's value.

The contract is `crates/config/src/coder_tuning_arrival_tests.rs`. Each row
writes one safety-critical `[coder]` field into a real config directory, loads
it through `load_config`, assembles the run config, and observes the changed
runtime field. Covered today:

| `tuning.toml` key | Silent-default cost if it does not arrive |
|---|---|
| `[coder.gate].enabled` | completion gate stays off |
| `[coder.coder].max_turns` | turn budget ignores the file |
| `[coder.progress].read_only_turn_limit` | progress limiter uses the code default |
| `[coder.hashline].enabled` | hashline edits stay off |

Extend that table one field at a time. Do not add another config framework.

## 5. Where the Tunables Come From

Each companion spec already defines its tunables and defaults in a "Tunables (single source of truth
— Decision 14)" table. Those tables are the **authoritative list**; the `crates/common` config model
mirrors them as typed fields with matching `Default`s. Concretely, the model aggregates:
- Capability/zone/write-class policy (Decision 4 / concurrency spec) → `policy.toml`.
- Concurrency: `WINDOW`, `MAX_REACTION_DEPTH`, `RETRY_MAX`.
- Dispatch: `SMALL_FANOUT`, tiered `CLARIFY_THRESHOLD`, `MAX_CONCURRENT_SUBAGENTS`,
  `DETACH_SOFT_TIMEOUT`, `dispatcher_model`, `guidance_match_floor`, `subagent.isolation`.
- Context policy: `MAX_GOALS`, `MAX_DECISIONS`, `DECISION_RECENCY`, `header_template`, `inbox_lookback`.
- Capture/inbox: paths, settle windows, flags, `ambient_sweep_schedule`, ignore globs.
- Maintenance/git: commit schedule, `stignore_machine_dirs`, maintenance schedule, prune/merge policy.
- Topology: component enablement, transports, ports/sockets, model/provider selection.

## 6. v1 Scope

- Typed config model in `crates/common` with `Default`s = specced defaults.
- Layered loader (defaults → files → env → CLI) producing one validated model.
- Cross-cutting validation + `liberado config check`.
- Three concern files; all optional (empty/absent config boots on defaults, modulo the minimum
  topology + policy a real deployment needs — e.g. at least one vault path and the daemon socket).

**Deferred (v2+):**
- **Vault-resident tunables** (edit benign knobs from Obsidian on the phone, validated-on-change).
- **User-approval-gated config changes through the system** (agents proposing config edits for
  human approval). Out of scope for initial work — config changes are manual via `ssh`.

## 7. Open Questions — resolved

Both questions below were open as of June 2026; the implementation (`crates/config`) has since
settled them.

1. ~~Config dir location: `/etc/liberado/` vs `~/.config/liberado/`~~ — **resolved**: `config_dir()`
   uses the platform config dir (`dirs::config_dir()/liberado` — XDG on Linux, `%APPDATA%\liberado`
   on Windows), with `LIBERADO_CONFIG_DIR` as an explicit override and a development-convenience
   fallback that walks up from the running binary looking for a `config/` directory. No separate
   `/etc/liberado/` path.
2. ~~Separate files vs sections of one file~~ — **resolved as separate**: `topology.toml`,
   `policy.toml`, and `tuning.toml` ship as three independent files, matching §3's layout.

## 8. Gitignore policy

Local deployment overrides are never committed; only the starter examples are:

| Path | Git status | Purpose |
|---|---|---|
| `config.example/` | committed | Starter files |
| `config/*.toml` | `.gitignore`d | Local deployment overrides |
| `crates/*/config.example/` | committed | Per-crate examples |
| `crates/*/config/*.toml` | `.gitignore`d | Per-crate deployment overrides |
