# Liberado — Simplification Opportunities (Round 2, amalgamated)

This artifact merges the second-round review from earlier in this session with
the parallel second-round review from a teammate's morning read. Each item
is tagged with its **status** (`DONE`, `PARTIAL`, or `NEW`) and a brief note
on whether it's been picked up by the `minimax-m3-simplifications` PRs
(`#256`–`#260`).

Items are grouped by **tier** — what's at the top costs least per line saved
and changes the fewest files. The numbers are roughly LOC you save, not
file count.

---

## Tier 1 — pure deletions, no behavior change

### #1. `crates/session/src/store.rs` (1040 lines) is a parallel store that almost no one uses — **NEW**
`session/src/store.rs` is an append-only JSONL `GoalSessionStore` whose own
header doc flags it as "deliberately a test double," yet it carries a full
disk format, rehydration, broadcast bus, per-session lock map, ID minting.
Roughly 80% of it duplicates `liberado-session-store::SessionStore` (a
1008-line file two crates over) just to offer a smaller type surface for
kernel tests. The 2026-07 conversation-store audit warned about exactly this
("a second implementation of a store, tested as if it were the real one")
and the kernel still has it.

After D7, the converged `liberado-session-store` exposes the same
`SessionRecordStore` trait lens the kernel needs. A 100-line in-memory
`HashMap<String, GoalSessionRecord>` test double (or a shim that strips
the DAG fields) is sufficient.

**Saves ~900 lines** and removes a "second implementation" risk class.
**Risk:** medium (kernel tests will need re-targeting; the in-memory
double is test-only and won't ship in production binaries).

### #2. Two `ScriptedToolRuntime` copies — **DONE**
`crates/heuristics-tuner/examples/debug_multistep.rs:13` now imports
`ScriptedToolRuntime` from the lib. Closed.

### #3. `crates/executor/src/lib.rs` local test doubles — **PARTIAL**
The local `MockToolRuntime` is gone (now `InvocationRecordingRuntime` from
`liberado-test-support`). `lib.rs` extracted its inline `#[cfg(test)]`
into `lib_tests.rs` (2585 lines) and `lib_survivor_tests.rs` (638).
`lib.rs` went from ~4800 to 2220 lines.

**Still local** (legitimately — they aren't direct test doubles):
- `SlowProvider` (wall-clock advance during a real call)
- `call_tool` / `call_tool_with` / `submit` builders
- `executor(...)` factory
- `offered_tools(provider)` accessor

These could move into `test-support` as builders but the gain is small
(~50 lines) and they read fine where they are.

### #4. `crates/session/src/life_demo.rs` (1354 lines) is mostly tests — **PARTIAL**
The actual `LifeOpsDemoRunner` is ~120 lines. The remaining ~1230 lines are
`impl DomainPackRunner for AlwaysResume / NeverResume / ResumablePack /
NeverEndingPack` test packs — fixtures dressed as runtime types so they
can sit inside the lib.

**Either:**
- Gate the file behind `#[cfg(test)]` (drops the fixture from production
  binaries, no test changes).
- Or move the demo packs into `liberado-test-support` next to
  `InvocationRecordingRuntime`.

**Saves ~1230 lines from production builds** (the `.text` segment of every
binary that pulls `liberado-session` shrinks correspondingly).

### #5. `crates/tui/src/commands.rs` — **DONE**
Verified gone. `crates/tui/src/` now contains `command_context.rs`, not
`commands.rs`. The 2026-07 audit's "Done" status was accurate.

---

## Tier 2 — collapse behind a trait, then delete types

### #6. Three `select_beam` / `advance_beam` triplets in `heuristics-tuner` — **NEW**
`crates/heuristics-tuner/src/`:

| Function | File:line | Tiebreaker after accuracy |
|----------|-----------|----------------------------|
| `select_beam` | `search.rs:89` | `safe_default_rate` |
| `select_beam_executor` | `tool_loop_search.rs:21` | `outcome_match_rate` |
| `select_beam_coder` | `coder_search.rs:23` | `nonempty_diff_rate`, then `outcome_match_rate` |

Each is the same body — disqualify on `unsafe_acts > 0`, sort by `accuracy`
desc, sort by tiebreaker desc, truncate — with only the tiebreaker field
differing. The `advance_beam_*` triplets right below are byte-identical
modulo the fitness type.

A `FitnessRank` trait with `unsafe_acts`, `accuracy`, and a single
`tiebreaker` f32 absorbs all three. ~120 lines + 3 test modules collapse.

**Risk:** low — pure refactor, no behavior change.

### #7. Three `run_*_tuner` loops with identical body shape — **NEW**
`run_tuner` (`search.rs:151`), `run_tool_loop_tuner` (`tool_loop_search.rs:169`),
`run_coder_tuner` (`coder_search.rs:149`) all do:

1. `Budget::new(config.call_budget)`
2. `score_X_candidate(seed_prompt, ..., &budget)` → `baseline_fitness`
3. `let baseline = Candidate { prompt: ..., origin: ColdStart };`
4. `let mut beam = vec![(baseline, baseline_fitness.clone())]`
5. `for generation in 0..config.max_generations:` — gather candidates,
   score pool, advance beam, justify, format rubric, push generation
6. Pick winner, finalize rubric, return

The team already extended `DomainGeneration` for `gather_generation_candidates`.
Adding `score_candidate` and `format_rubric` to that trait (plus a
`GenerationRecord<F>` / `TunerResult<F>` generic, leveraging #6's
`FitnessRank`) collapses the bodies to one generic `run_tuner<G>`. ~180
lines.

**Risk:** medium — these are the public entry points. The trait extension
is additive, but the call site churn is three modules.

### #8. Three `format_*_rubric` / `finalize_result_*` formatters — **NEW**
`format_rubric` (dispatcher) vs `format_executor_rubric` (tool-loop) vs
`format_coder_rubric` (coder) all do the same thing: build a markdown
table from the current fitness's pass_rate/accuracy, the baseline's, and
the named failure list. They differ only in column choice and prose.
`finalize_result` / `finalize_result_executor` / `finalize_result_coder`
all do "use the last generation's rubric if any, otherwise format a fresh
one." ~100 lines plus three test files.

A `RubricFormatter` trait on top of `DomainGeneration` absorbs all three.

**Risk:** low — pure formatters, isolated.

### #9. ~5 cheap `ToolRuntime` wrappers still hand-rolled — **PARTIAL**
The audit's item #5 ("13 wrappers") is well underway. `tool-runtime/src/decorating.rs`
already implements the generic wrapper; `mcp/scoped.rs::ScopedRuntime` and
`main-agent/sessions/grants.rs` already use it.

**Still hand-rolled and candidates for `DecoratingRuntime`:**

| Wrapper | Location | Notes |
|---------|----------|-------|
| `AsToolRuntime` | `mcp/src/pool.rs:397` | pure passthrough → `DecoratingRuntime::passthrough` |
| `PassThroughRuntime` | `main-agent/src/sessions.rs:2031` | pure passthrough |
| `NoToolsRuntime` | `main-agent/src/sessions.rs:2043` | empty catalog (filter=always false) |
| `RiskGatedToolRuntime` | `executor/src/risk_gated.rs:291` | needs the gate lifted into a free function |

`MultiMcpRuntime` (`mcp/src/multi.rs:25`) and `PooledCheckout`
(`mcp/src/pool.rs:355`) stay bespoke — routing by `<server>:<tool>` prefix
and pool health reporting are more than one decorator can absorb. ~400 lines
of cheap wrappers fold in.

**Risk:** low.

### #10. Provider factory chain — **PARTIAL**
The audit's item #4 was about `OpenAiCompatibleProvider::with_overrides`,
which is **done** (`provider-openai-compat/src/lib.rs:141`, used by
`bootstrap/src/lib.rs:108-110`, `coder-runner/src/main.rs:686-691, 795-798`).

The companion half — *one* `ProfileProviderFactory` shared by five call sites —
isn't done:

| File | Struct |
|------|--------|
| `bootstrap/src/lib.rs:54` | `provider_from_config` + `build_provider_from_profile` |
| `bootstrap/src/lib.rs:200+` | `CoderRoleProviderFactory::provider_for` |
| `coder-runner/src/main.rs:680` | `DirectProviderFactory` |
| `coder-runner/src/main.rs:789` | `OpenAiProfileProviderFactory` |
| `acp-bridge/src/provider.rs:90` | `provider_from_liberado_config` |

One generic `ProfileProviderFactory { profile, api_key }` in
`liberado-bootstrap` collapses all five.

**Risk:** medium — composition roots, but the chain is now uniform.

### #11. Three `Provider` wrappers each hand-write the trait surface — **NEW**
| Wrapper | Location | What it adds |
|---------|----------|--------------|
| `MeteredProvider` | `provider/src/latency.rs:150` | records latency events on every call |
| `RoleBoundProvider` | `acp-bridge/src/coding_run.rs:722` | per-call `.with_model().with_reasoning()` rewrite |
| `MissingKeyProvider` | `acp-bridge/src/provider.rs:284` | a keyless placeholder for Paseo detection |

Each hand-writes `model / complete / complete_stream / list_models /
set_model` with the same pattern: do per-call work, forward to `self.inner`.
~80 lines each.

A `provider::Layer` primitive (analog of `DecoratingRuntime` for
`Provider`) would let new wrappers add ~20 lines instead of ~80. The
`DecoratorRuntime` model from #9 doesn't quite fit because `Provider` has
both a `complete` and a `complete_stream` path that need to be layered
together, and `MeteredProvider` adds a *post*-invoke observe hook that the
current `DecoratingRuntime` doesn't have.

**Risk:** medium — the trait surface is wider than `ToolRuntime`, so the
foundation primitive needs more design.

### #12. `server/src/state.rs::NoTools` and two other production-side clones — **NEW**
Three remaining production `NoopRuntime` clones:

| Location | Notes |
|----------|-------|
| `server/src/state.rs:153` | real runtime when no MCP is configured |
| `acp-bridge/src/main.rs:161` | test/runtime stub |
| `main-agent/src/sessions.rs:1999` | `NoToolsRuntime` (yet another spelling) |

All three can become `liberado_test_support::InvocationRecordingRuntime`
constructed with `with_default_result(Err("…"))` and an empty catalog.
The first one (`server/src/state.rs`) is real production code with a
custom error string — the conversion is one call site. The other two are
5-line structs.

**Risk:** very low.

### #13. Two `MissingKeyProvider`-style placeholders in `acp-bridge` — **NEW**
`acp-bridge/src/provider.rs:284` (`MissingKeyProvider`) and the
keyless-placeholder path in `provider_from_env_profiles` (same file,
lines 122–160) both build "a provider with no key that won't actually be
called but exists so Paseo can detect us." Same idea, two paths. A single
`KeylessPlaceholder::new(backend_name, base_url)` returned by a
`bootstrap::placeholder_provider()` helper absorbs both. ~30 lines.

### #14. `cold_start` helper across three generation modules — **NEW**
`generation.rs::cold_start`, `coder_generation.rs::cold_start_coder`,
`tool_loop_generation.rs::cold_start_executor` all build the same shape:

```
let request = CompletionRequest::new(vec![
    Message::system(META_SYSTEM_PROMPT),
    Message::user(format!("{TASK}\n\nWrite the system prompt now.")),
])
```

…then budget-check, call `complete_json`, surface `GenerationError`. The
`META_SYSTEM_PROMPT` / `TASK` strings are the only layer-specific bits. A
shared `cold_start_with(system, task, budget)` helper collapses the body;
each layer module keeps its own constants and a one-line wrapper.

~40 lines.

**Risk:** low.

---

## Tier 3 — crate architecture (bigger moves, separate PRs each)

### #15. `liberado-common` (22 files, 6240 lines) is nine unrelated concerns — **NEW**
This is the deferred item from `docs/future-work/archive/crate-modularity-audit.md`'s
"only fully open item." It is still open.

The natural splits (each topic has its own coherent vocabulary; together
they form a grab-bag every other crate compiles against):

| New crate | Source files |
|-----------|--------------|
| `liberado-permissions` | `capability.rs` (1389), `catalog.rs` (1054), `risk_waiver.rs`, `sweeping.rs` |
| `liberado-proposals` | `proposal.rs` (852), `proposal_summary.rs`, `approval_ledger.rs` (227), `frontmatter.rs` |
| `liberado-dispatch-types` | `dispatch.rs` (570), `event.rs`, `process.rs`, `local_time.rs`, `clock.rs`, `model.rs` |
| `liberado-provenance` | `provenance.rs` (134), `path.rs` |

The layer rules are now stable enough that the split is purely mechanical.
The big blast radius is the dependency rewrite (~30 crates) but each
crate's imports are usually a small fixed set, so a `cargo fix` pass
should land most of it.

**Risk:** large. Defer until the rest is in.

### #16. `liberado-config` and `liberado-config-loader` are two names for one crate — **NEW**
`crates/config-loader/` (835 lines of source, plus `model/` at 5525) and
`crates/config/` (1384-line lib.rs) had a cycle-avoidance reason for the
split, but the public surface is already unified: `config/src/lib.rs:32-41`
does `pub use liberado_config_loader::{Config, Topology, Policy, Tuning, …}`
and the bootstrap layer re-exports the same list. Nobody imports
`liberado-config-loader::*` directly except the loader's own tests.

Two options:
1. **Just merge them.** The cycle resolves if `validate_merged_config`
   lives in a small sub-module that `model/` already depends on.
2. **Keep the split but rename.** Fold `config/src/lib.rs` into
   `config-loader`, delete one Cargo manifest.

Either way you delete one Cargo manifest, one set of tests, and one layer
of re-exports.

**Risk:** low — no behavioral change, just a cleanup of `pub use` chains.

### #17. `liberado-coder-core/control_plane/` should be its own crate — **NEW**
`crates/coder-core/src/control_plane/` is 2382 lines
(`record.rs:535`, `opencode.rs:444`, `config.rs:381`, `ledger.rs:380`,
`supervisor.rs:273`) owning a durable task ledger, an `OpenCodeWorker`
adapter, supervisor state machines, and a `ContinuationContextBuilder`.
None of it is used outside `liberado-coder-agent` and `liberado-coder-runner`.

The `liberado-coder-core` description says it "intentionally owns no
implementation," and then `control_plane/` breaks that promise — it's
effectively a second crate hiding inside one. Moving it to
`liberado-coding-control-plane` shrinks `coder-core`'s lib.rs from 1371
to ~400 lines and the "no implementation" promise becomes true again.

**Risk:** medium (only two consumers; a contained refactor).

### #18. `liberado-coder-sandbox` and `liberado-coder-tools` git code split — **NEW**
The split is meaningful — sandbox is sandbox, tools are tool impls — but
`coder-tools/src/git.rs` and `coder-tools/src/git_gix.rs` (588 lines
total) wrap `gix` next to `coder-sandbox/src/merge.rs` which is also
git/worktree code. The `git.rs` shim is just "if no `git` feature, return
error," and the rest of `git_gix.rs` is `gix` operations.

Move `git.rs` + `git_gix.rs` into `coder-sandbox` and rename it
`coder-runtime` (workspace + git + the `Workspace`/`Sandbox` traits that
the tools actually consume). `coder-tools` becomes only the tool
definitions — which is what the name implies.

**Risk:** low (one consumer of the git code outside `coder-tools`).

### #19. `liberado-coder-runner` re-implements bootstrap's profile lookup — **NEW**
`crates/coder-runner/src/main.rs:716-749` defines `provider_profile`,
`provider_profile_named`, and `read_topology` — a re-derivation of
`bootstrap::provider_from_config`. The doc comment at line 727 says
"the pure provider-lookup half … with the provider name supplied as an
argument instead of read from the environment," which is a fair
factoring, but the same pattern exists in `liberado-bootstrap`.

`read_tuning` (line 751) has the same problem — it's a hand-rolled TOML
reader because the runner wants only `tuning.toml`. But
`crates/config/src/lib.rs:186` already exports `load_tuning_only(dir)` for
exactly this.

Either:
- `coder-runner` should call `liberado_bootstrap::provider_from_config` and
  pass the result through a thin factory (combined with #10's
  `ProfileProviderFactory`).
- Or `bootstrap` should grow a
  `provider_from_config_with_profile_override(profile_name)` API that
  `coder-runner` can use directly.

`read_tuning` becomes `liberado_config::load_tuning_only(dir).map_err(...)?`.
~80 lines vanish.

**Risk:** low (mechanical replacement).

---

## Tier 4 — already done in this round, verify and document

### #20. `OpenAiCompatibleProvider::with_overrides` collapse — **DONE**
`crates/provider-openai-compat/src/lib.rs:141` + the inline references
in `bootstrap/src/lib.rs:108` and `coder-runner/src/main.rs:688`. The
remaining cleanup is #10.

### #21. `DecoratingRuntime` — **DONE** (consumers in progress)
`crates/tool-runtime/src/decorating.rs` already implements the generic
wrapper. Consumers need to switch — that's #9.

---

## Suggested sequencing

Following the 2026-08 audit's ordering, plus the new items, prioritized by
LOC saved per review risk:

1. **#2** (delete `examples/debug_multistep.rs`'s duplicate `ScriptedToolRuntime`) — already done. Verify and close.
2. **#5** (TUI `commands.rs`) — already gone. Verify the 2026-07 audit's claim was accurate; no work needed.
3. **#14** (cold_start helper) — 30 minutes, mechanical, pure refactor.
4. **#12** (the last three `NoTools` clones) — 30 minutes, mechanical.
5. **#13** (`KeylessPlaceholder` helper) — small, folds in with #10.
6. **#4** (`life_demo.rs` test packs behind `#[cfg(test)]`) — purely additive gate, no test changes.
7. **#19** (`coder-runner` profile lookup → `bootstrap`) — removes ~80 lines of forked logic.
8. **#1** (delete `session/src/store.rs` parallel store; keep a 100-line in-memory test double) — biggest LOC win that doesn't require new abstractions.
9. **#6** (`select_beam` generic) — 30 minutes, pure refactor; lands first because #7 builds on the same trait.
10. **#8** (`format_rubric` trait) — same scale, same purity. Same PR as #6 if "tuner formatting" sounds right.
11. **#7** (`run_*_tuner` generic) — depends on #6's trait landing first. Biggest win of this round.
12. **#10** (one `ProfileProviderFactory` in `bootstrap`) — unlocks future per-role knobs.
13. **#9** (route cheap `ToolRuntime` wrappers through `DecoratingRuntime`) — ~400 lines, but each wrapper is its own PR-sized commit.
14. **#11** (`Provider::Layer`) — small foundation crate or extension; its own PR.
15. **#16** (merge `liberado-config` + `liberado-config-loader`) — one Cargo manifest gone.
16. **#17** (control_plane out of `coder-core`) — contained, ~2382 lines moved.
17. **#18** (`coder-sandbox` + `coder-tools` git rebalance) — one consumer of the git code outside `coder-tools`.
18. **#15** (`liberado-common` split) — the largest blast radius. Defer until the rest is in.

The two I'd push hardest to do next are **#1** (parallel session store) and
**#19** (`coder-runner` profile lookup). Both are deceptively "scary" — the
session store has a doc comment begging to be deleted, and #19 is a
straightforward replacement of forked code with the canonical helper.
The rest are mechanical.
