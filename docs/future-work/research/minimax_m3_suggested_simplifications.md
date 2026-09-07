# Liberado — Simplification Opportunities

After reading through the 53-crate / 257K-line Rust workspace, the codebase is
genuinely well-architected (mechanically-enforced layer rules, `test-support`
crate, centralized `MockProvider`). The duplication I found is mostly in
*test doubles* and in *two pieces of "glue" logic that composition roots each
re-derive*. Almost none of it would change observable behavior.

The recommendations are ordered roughly by the LOC you'd save and the
risk/reward of the change.

---

## 1. ~21 duplicate `NoopRuntime` / `NoTools` / `MockInner` test doubles

**Size:** ~250–400 lines of test-only code, plus the noise of reading it.
**Risk:** very low — these are all behaviorally identical to what
`liberado-test-support` already exports.

**Evidence.** `grep "struct NoopRuntime\|struct NoTools\|struct NoopRt\|struct MockInner\|struct MockRuntime"`
turns up 21 definitions across these files:

```
crates/acp-bridge/src/main.rs:161
crates/daemon/src/tests/guards.rs:53,128,204         (NoopRt, NoopRt2, NoopRt3 — three near-clones)
crates/daemon/src/tests/test_fixtures.rs:45
crates/dispatch-pack/src/lib_tests.rs:30
crates/executor/src/risk_gated_survivor_tests.rs:12
crates/executor/src/risk_gated_tests.rs:6
crates/main-agent/src/lib.rs:405
crates/main-agent/src/lib_tests.rs:13
crates/main-agent/src/sessions/test_fixtures.rs:18
crates/main-agent/src/sessions.rs:2043                (NoToolsRuntime — yet another spelling)
crates/mcp/src/multi.rs:91                            (records invoke, has tools catalog)
crates/mcp/src/pool.rs:446
crates/mcp/src/scoped.rs:112                          (records invoke, has tools catalog)
crates/server/src/api/chat_endpoint_tests.rs:10
crates/server/src/cron_delivery_delivery_tests.rs:141
crates/server/src/shutdown_tests.rs:20
crates/server/src/state.rs:153
crates/server/src/telegram_tests.rs:60
crates/test-support/src/lib.rs:59                     (the canonical one)
```

Every one of them is `fn catalog(...) -> Vec::new()` plus
`async fn invoke(_) -> Ok("ok")` (or `Err("no tools")` — a difference
that callers could absorb by choosing one or by parameterizing).

**What to do.** Two changes:

1. Make `liberado_test_support::NoopRuntime` (and `InvocationRecordingRuntime`,
   already there) `pub`-reexported, and have every crate listed above
   `use` it. The `pool.rs` and `mcp/` variants additionally need
   `RebindableRuntime` — that's a one-method trait in `liberado-executor`,
   so `test-support` can blanket-impl it for `NoopRuntime`.

2. For the **recording-with-tools** flavor (e.g. `MockToolRuntime` in
   `executor/src/lib.rs:2200`, `MockInner` in `mcp/scoped.rs:112` and
   `mcp/multi.rs:91`), the existing `InvocationRecordingRuntime` plus
   a thin `with_catalog(&[&str])` builder would replace all three.
   These three are the same struct: `tools: Vec<ToolDef>` +
   `invoked: Mutex<Vec<ToolInvocation>>` + `result: Result<String,String>`.

The `MockToolRuntime` in `executor/src/lib.rs:2200–2233` is especially
cheap to delete: its own crate owns the `ToolRuntime` trait, so it can
depend on `liberado-test-support` (it already pulls `liberado-provider`).

The three "guarded tracing" `NoopRt1/2/3` in
`daemon/src/tests/guards.rs` are an interesting microcosm — they're
distinguished only by what tool they reject. They could become
`NoopRuntime::default().with_error("x", "denied")` and the test
narrows accordingly.

---

## 2. Two `ScriptedToolRuntime` definitions in the same crate

**Size:** ~80 lines.
**Risk:** zero — one is a copy of the other.

`crates/heuristics-tuner/src/tool_loop_scoring.rs:24–67` and
`crates/heuristics-tuner/examples/debug_multistep.rs:20–67` are
textually identical except that the example writes
`std::collections::HashMap` inline instead of importing the alias.

The example should just `use` the lib's `ScriptedToolRuntime`. (The
in-tree docstring on the lib version literally says it's a scenario
fixture — exactly the example's use case.)

---

## 3. Three `ScoredScenario` / `*Fitness` shapes in `heuristics-tuner`

**Size:** ~1700 lines, with strong structural overlap.
**Risk:** medium — these are public-ish types consumed by `lib.rs` re-exports
and CLI output. A refactor is doable but needs to keep the public shape.

**Evidence.** Three near-identical scoring modules:

| File | Score type | Trial type | Outcome type | Fitness type |
|------|------------|------------|--------------|--------------|
| `scoring.rs` | `ScoredScenario` | `ScenarioTrial` | `ScenarioOutcome` | `CandidateFitness` |
| `tool_loop_scoring.rs` | `ToolLoopScoredScenario` | `ToolLoopTrial` | `ToolLoopOutcome` | `ToolLoopFitness` |
| `coder_scoring.rs` | `CoderScoredScenario` | `CoderTrial` | `CoderTrialOutcome` | `CoderFitness` |

All three expose the same methods:
`pass_rate()`, `any_unsafe()`, `trial_breakdown()`,
`diagnostic_breakdown()` (tool_loop and coder only), plus
`*_match_rate()` / `nonempty_diff_rate()` / `safe_default_rate()`
specific to the layer. The `aggregate()` functions are all
`mean-of-pass_rate, mean-of-some-rate, count-of-any_unsafe`.

The `ScoredScenario` struct fields are also the same shape:
`name`, `goal`/`task`, `note`, `expected`/`expect`, `trials`.

**What to do.** Lift to a single generic:

```rust
pub struct ScoredScenario<O> {
    pub name: &'static str,
    pub description: &'static str,   // was goal / task
    pub note: &'static str,
    pub trials: Vec<Trial<O>>,
}

impl<O: OutcomeLike> ScoredScenario<O> {
    pub fn pass_rate(&self) -> f32 { ... }
    pub fn any_unsafe(&self) -> bool where O: HasUnsafe { ... }
    pub fn trial_breakdown(&self) -> String where O: HasPass { ... }
    pub fn diagnostic_breakdown(&self) -> String where O: HasDiagnosticRates { ... }
}
```

`O` is a small trait that exposes the bits the generic methods need
(`pass()`, `unsafe_flag()`, plus optional rate-flavored methods). The
layer-specific extras (`safe_default_rate`, `nonempty_diff_rate`,
`outcome_match_rate`) stay as free functions or extension traits on
each outcome type. `aggregate()` becomes a generic too.

The cost: ~600 lines collapse to ~300, and a new layer (orchestrator,
evaluator, anything else) gets scoring for free. The benefit compounds
when the next `*_scenario.rs` / `*_generation.rs` / `*_search.rs` triplet
arrives — those three modules have the same parallel structure
across the same three layers (see `coder_scenarios.rs` /
`tool_scenarios.rs` / `scenarios.rs` and the `*_generation.rs` /
`*_search.rs` siblings).

---

## 4. Three near-identical provider factories

**Size:** ~150 lines, but more importantly three places where adding a
"new override knob" needs to be done in three different files.
**Risk:** medium — these are composition roots, behaviorally
load-bearing. The fix is small and the gain is structural.

**Evidence.** Three places construct an `OpenAiCompatibleProvider`
from a `ProviderProfile` and apply a `(model, reasoning, temperature)`
override:

| File | What it does |
|------|--------------|
| `bootstrap/src/lib.rs:93` (`build_provider_from_profile`) | `OpenAiCompatibleProvider::from_env(...).with_temperature(ov.temperature).with_reasoning_effort(ov.reasoning.map(...))` |
| `bootstrap/src/lib.rs:146–167` (`CoderRoleProviderFactory::provider_for`) | Same call, but per-role — duplicates the same `with_*` chain |
| `coder-runner/src/main.rs:680–691` (`DirectProviderFactory`) | Inline: `OpenAiCompatibleProvider::new(...).with_extra_client_error_status(...).with_reasoning_effort(...)` |
| `coder-runner/src/main.rs:786–797` (`OpenAiProfileProviderFactory`) | Same, from a `ProviderProfile` |
| `acp-bridge/src/provider.rs:69–119` (`provider_from_liberado_config`) | `OpenAiCompatibleProvider::from_env(...).set_model(...)` |

All five should converge on one struct in `liberado-bootstrap` (or
`liberado-provider-openai-compat`, where the chain originates) like:

```rust
pub struct ProfileProviderFactory { profile: ProviderProfile, api_key: Arc<str> }
impl CoderProviderFactory for ProfileProviderFactory { ... }
```

…and the handful of call sites collapse to a one-liner. The "model /
reasoning / temperature" override logic, currently written five times
with slight variation, becomes one method. The current
`build_provider_from_profile` in `bootstrap` would *also* use it for
the daemon's primary provider, which is the right fix for the
`acp-bridge`'s special case (it currently re-walks the profile to
rebuild what the daemon's bootstrap already built).

---

## 5. ~10 `ToolRuntime`-wrapping structs that could share a decorator

**Size:** ~600 lines and a lot of near-identical
`async fn invoke { self.inner.invoke(...) }` boilerplate.
**Risk:** low — behavior would not change.

**Evidence.** These structs all do "wrap another `ToolRuntime`, add
some behavior around `invoke`":

| Type | Location | What it adds |
|------|----------|--------------|
| `AsToolRuntime` | `mcp/pool.rs:397` | passthrough (used to erase the trait object) |
| `PermittedRuntime` | `mcp/pool.rs:410` | passthrough + permit handling |
| `ScopedRuntime` | `mcp/scoped.rs:40` | filters catalog + invocations |
| `MultiMcpRuntime` | `mcp/multi.rs:25` | routes by `<server>:<tool>` prefix |
| `PooledCheckout` | `mcp/pool.rs:355` | delegates + reports health to pool |
| `PassThroughRuntime` | `main-agent/sessions.rs:2031` | passthrough |
| `NoToolsRuntime` | `main-agent/sessions.rs:2043` | passthrough |
| `FaceRuntime` | `main-agent/face.rs:226` | projects tool results through a face/transform |
| `GuardedTracingRuntime` | `coder-agent/runtime.rs:21` | progress guards + tracing |
| `RiskGatedToolRuntime` | `executor/risk_gated.rs:291` | zone/capability gate |
| `AskHumanRuntime` | `acp-bridge/ask_human.rs:125` | per-tool router |
| `DoneRuntime` | `acp-bridge/done.rs:144` | per-tool router |
| `NoTools` | `acp-bridge/main.rs:164` | catalog=[] + invoke error |

Most of these are "delegate everything to `inner`, do a tiny bit
extra". A generic decorator would handle ~70% of them:

```rust
pub struct DecoratingRuntime<F> {
    inner: Arc<dyn ToolRuntime>,
    pre_invoke: F,    // Fn(&ToolInvocation) -> Option<Result<String, String>>
}
```

`AsToolRuntime`, `PassThroughRuntime`, `NoToolsRuntime`, and the
catalog filters in `ScopedRuntime` all reduce to
"pre_invoke returns None; call inner". The interesting wrappers
(`RiskGated`, `GuardedTracing`, `PooledCheckout`, `ScopedRuntime::invoke`'s
rejection path) become "pre_invoke returns Some(...) when blocked;
otherwise call inner + post_invoke runs afterwards".

The win isn't LOC; it's that today each new wrapper hand-writes
`async_trait` boilerplate, its own test fixtures, and a new
`impl ToolRuntime` — a generic decorator turns that into one new
struct per *idea*, not per *file*.

---

## 6. A few smaller things

- **`provider-free-proxy` is 5K lines of test coverage for ~1K of logic.**
  The `resolver_tests.rs` (801 lines) and `match_slug_tests.rs` (129)
  together almost equal the prod code they cover. Nothing duplicated,
  but worth checking whether all the table-driven cases are pulling
  weight or are paper coverage.

- **`session/src/life_demo.rs` is 1,354 lines and is mostly a
  test-fixture file disguised as a "demo pack".** Lines 850–1354 are
  `impl DomainPackRunner for AlwaysResume / NeverResume / ResumablePack
  / NeverEndingPack` — the actual `LifeOpsDemoRunner` is only ~120
  lines. The rest could be `#[cfg(test)] mod tests` and the file
  shrinks by 90%.

- **`executor/src/lib.rs` is 4,800+ lines.** Half of it is the inline
  `#[cfg(test)]` block (the `MockToolRuntime`, `SlowProvider`,
  `offered_tools`, `call_tool` helpers, and dozens of `executor(...)`
  builders all live there). `test-support` already has equivalents
  for most of them; pulling those out would be a single `mod tests {`
  move and a `use` change, no test logic altered.

---

## What I would *not* consolidate

A few things look duplicate at a glance but aren't worth touching:

- **`HeuristicsTuner` per-layer `*_scenarios.rs` / `*_generation.rs` /
  `*_search.rs` triplets.** They share a *shape* but the contents are
  domain-specific prompts, schemas, and scoring code. Collapsing them
  to a generic would obscure the per-layer prompt design. The
  `ScoredScenario` collapse (item 3) is the right level.

- **`MockProvider` is already centralized.** Don't touch.

- **`ChannelNotifier` and `TelegramNotifier` look like they might
  overlap with `MessagingChannel`.** They don't — one is a one-way
  `Notifier`, the other is duplex, and `ChannelNotifier` already
  composes them properly.

- **`coder-agent/src/runtime.rs::GuardedTracingRuntime` looks like
  the `Trace` pattern in `test-support::trace_contracts`.** It isn't
  — one is production (events into a session), the other is
  testing-only (parses MVL logs off disk). The names collide; the
  responsibilities don't.

---

## Suggested order of attack

1. **#2** (delete the example copy) — 5 minutes, zero risk.
2. **#1** (route every test's `NoopRuntime` / `MockInner` /
   `MockToolRuntime` through `liberado-test-support`) — mechanical,
   200–400 lines gone, every test still passes.
3. **#6** last bullet (move `executor/src/lib.rs`'s `#[cfg(test)]`
   into a `mod tests` and into `test-support` where applicable) —
   same shape as #1.
4. **#4** (single `ProfileProviderFactory` shared by bootstrap /
   coder-runner / acp-bridge) — small, removes a class of
   drift-bug, unlocks future "per-role override" additions being
   one-liners.
5. **#5** (generic `DecoratingRuntime`) — only after #1 lands,
   because it's the same pattern of "wrap a `ToolRuntime`" and
   should compose with the consolidated test doubles.
6. **#3** (single generic `ScoredScenario<O>` in
   `heuristics-tuner`) — biggest payoff, biggest design surface;
   the type needs to stay public because the lib re-exports it.
   Worth doing as a separate PR so its diff is reviewable on its
   own.
