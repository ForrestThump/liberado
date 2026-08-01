# Hygiene audit — dedup, coupling, decomposition, design patterns, coverage (2026-07-04)

Four parallel subagent passes plus two tool runs (`cargo dupes`, `cargo llvm-cov`) across the whole
`crates/` workspace. This is an audit, not a changelog — nothing here has been fixed yet except where
explicitly marked. Priorities reflect actual risk/payoff, not raw finding count.

## Priority 1 — worth fixing soon

**Status: all four items below (the three Priority 1 fixes plus the `common::config` split pulled
forward from Priority 2) have been fixed** — `fe2c1fe` (proposal-write bug), `c149240` (the two
panics), `b6238e0` (provider dedup), and the `common::config` → `config-loader` split (this commit).
Each was built and full-workspace-verified independently, in that order.

### A real bug: a failed proposal write is silently reported as success

`crates/executor/src/risk_gated.rs`'s `write_proposal` (~lines 204-229): if `create_dir_all` or
`fs::write` fails, the error is logged but the function still returns a `PathBuf` and execution
continues as if the write succeeded. The caller composes a "PROPOSAL CREATED — saved at …" message
from that path regardless. Since the whole propose→approve→execute safety loop (Decision 11) depends
on a human actually seeing and approving a real file, a silent write failure here means the system
tells the user something was saved for review when nothing was — and approval can never arrive. Fix:
change the return type to a `Result`, propagate the failure as a real error instead of a fabricated
success path.

### Two panics reachable in production, not just theoretically

- `dispatcher/src/lib.rs:296,314` — `downgrade_to_propose_tool_calls`/`downgrade_to_propose_subagent`
  use `unreachable!()` on a `let...else` branch that's only actually unreachable because of a
  precondition maintained by the *caller's* match guard, not by the type system. A future change to
  `downgrade()`'s call sites could violate that precondition and panic in production. Fix: since these
  are private helpers, take the specific payload type directly instead of the whole
  `DispatchDecision`, so the precondition becomes a compile-time signature match instead of a runtime
  assumption.
- `orchestrator/src/lib.rs:455` — `semaphore.clone().acquire_owned().await.unwrap()` in
  `dispatch_parallel`. Only panics if the semaphore is closed (doesn't happen today), but it's an
  unchecked `unwrap` outside tests. At minimum `.expect("semaphore closed — this is a bug")` so a
  panic here is legible instead of a bare unwrap trace.

### Provider duplication: `provider-deepseek` and `provider-openrouter` are ~90% the same code

Confirmed line-by-line: `to_openai_request`, `message_to_json`, `tool_to_json`,
`accumulate_tool_deltas`, `parse_tool_call`, `from_openai_response`, `parse_usage`,
`build_tool_name_map`, `basic_sanitize`, `ToolAcc`/`ToolNameMap`, and the entire `Provider` trait impl
(`complete()` 27 lines, `complete_stream()` 77 lines including the full SSE assembly) are byte-for-byte
identical between the two crates. Only four things genuinely differ: `DEFAULT_BASE_URL`,
`DEFAULT_MODEL`, the `from_env()` env-var names, and `map_status()`'s handling of OpenRouter's extra
`402` (insufficient credits) case.

**Recommended fix**: add `pub mod openai_compat` inside `crates/provider/src/` (the narrow-waist crate
both backends already depend on — no new dependency edge). Move every function listed above in;
`provider-deepseek`/`provider-openrouter` keep only their constants, `new()`/`from_env()`/
`with_base_url()`/`endpoint()` (differ by env-var name), the `Provider` impl bodies (they close over
`self.client`/`self.api_key`, so they stay, but now call into the shared module), and `map_status()`.
Low risk — everything moving is pure, already-tested, side-effect-free logic. Real payoff: a parsing
bug currently needs fixing twice, and a third OpenAI-compatible backend (OpenAI direct, Groq,
Together) would otherwise be a third copy.

## Priority 2 — worth doing, lower urgency

### `liberado-common` split: `config` module is the best next carve-out — **done**

Module usage across the workspace: `config` (14 crates, 967 lines) and `catalog` (13 crates) dominate;
`capability` (11 crates) and `catalog` are genuinely coupled (`catalog.rs` imports `Consequence` from
`capability`) and should stay together. `proposal` (480 lines, 6 crates — daemon/executor/main-agent/
dispatcher/config/eval) has no dependency on `capability`/`catalog` and is cleanly separable — a
`liberado-proposal` crate is a reasonable second carve-out (still open). `event` (145 lines, 1 crate —
`daemon` only) should stay put until hooks/cron actually land and give it more consumers, per the
doc's own stated plan. **`model` (`ModelProfile`/`ModelRole`) checked (2026-07-07) — not dead
weight**: `config-loader`'s `Config.topology.models`/`model_roles` (the Decision-13 model-tier/role
floor system) is a real consumer (`crates/config-loader/src/model.rs`), just one hop removed from
the obvious grep since it goes through `liberado_config`'s re-export rather than a direct
`liberado_common::model` import in a leaf crate. No action needed here.

`config`'s case: `common::config`'s 967-line `Config`/`Topology`/`Policy`/`Tuning` type model overlapped
semantically with what `crates/config` already validated and `crates/config-loader` already
deserialized — moved 2026-07-04. It landed in `liberado-config-loader`, not the initially-obvious
`crates/config`, because `liberado-config-loader`'s own cross-cutting validation
(`validate_merged_config`) needs the type, and `liberado-config` already depends on
`liberado-config-loader` for that function — putting the model in `liberado-config` instead would
have created a cycle. `liberado-config` re-exports everything, so the external-facing import path
(`liberado_config::Config`) is unchanged. This also removed the prior oddity of `mcp-forge` depending
on all of `liberado-common` just to reach `Zone`/`managed_binary_path()` (it now reaches
`managed_binary_path` via `liberado-config`, which it already depended on).

### `heuristics-tuner`: don't split the crate, but split the module

The dispatcher-tuning and executor/subagent-tuning logic currently live flat in the same files
(`search.rs`'s `select_beam`/`select_beam_executor`, `scoring.rs`'s `CandidateFitness`/
`ToolLoopFitness`, `generation.rs`'s `cold_start`/`cold_start_executor`) — this was a deliberate,
documented tradeoff this session (isolating the new, unproven executor-tuning logic from the
just-fixed dispatcher path), not an oversight. Now that both paths are proven live, the flat structure
makes it hard to tell at a glance which half of a file applies to which layer. Recommendation: fold
the executor/subagent-tuning code into its own module (e.g. `src/tool_loop_tuning/`) rather than a
separate crate — `Budget`/`Candidate`/`TunerConfig` are genuinely shared scaffolding that a full crate
split would have to duplicate or re-thread. A one-afternoon internal reorganization, not urgent.

**Resolution (2026-07-07):** Done, matching the recommendation exactly — `scoring.rs` turned out to
already be properly split (`ToolLoopFitness` already lived in its own `tool_loop_scoring.rs`, from
before this audit), so only `search.rs` and `generation.rs` needed the cut. Moved
`select_beam_executor`/`advance_beam_executor`/`ExecutorGenerationRecord`/`ExecutorTunerResult`/
`run_executor_tuner`/`run_subagent_tuner`/`run_tool_loop_tuner` into a new `tool_loop_search.rs`,
and `cold_start_executor`/`mutate_executor` into a new `tool_loop_generation.rs` — flat files
alongside the existing `tool_loop_scoring.rs`/`tool_scenarios.rs`, not a `tool_loop_tuning/`
subdirectory, to match that established naming convention. `request_justification_if_budget_allows`
(in `search.rs`) and `request_justification`/`schema`/`PromptOutput` (in `generation.rs`) stayed put
and went `pub(crate)` — genuinely shared by both layers, not executor-specific. Verified: all 83
tests redistribute correctly with zero count change (`cargo test -p liberado-heuristics-tuner`),
clean `cargo clippy -p liberado-heuristics-tuner --all-targets`.

### Test extraction from two oversized files

`tui/src/app.rs` (2527 lines) and `main-agent/src/sessions.rs` (1081 lines) are each roughly half
production code, half `#[cfg(test)]` module. Neither is a god-file — the logic itself is cohesive —
but extracting the test modules into `tests/` (or a `src/app/tests.rs` submodule) would substantially
improve navigability for a cold read. Routine housekeeping, not urgent.

**Resolution (2026-07-07):** Done via the `src/app/tests.rs`-submodule option, not a top-level
`tests/` integration directory — both test modules use `use super::*;` to reach private
items (`App`'s private fields/helpers, `ChatSessions`'s internals), which a real top-level `tests/`
integration test can't access at all (it's a separate compilation unit, public-API-only). A
`#[path = "app/tests.rs"] mod tests;` (and the `sessions/` equivalent) keeps the exact same
compilation semantics — still a private inline module, just physically relocated — so this is a
pure file-layout change, zero behavior difference. `app.rs` 2514 → 658 lines (production only),
new `app/tests.rs` 1857 lines; `sessions.rs` 1125 → 571 lines, new `sessions/tests.rs` 555 lines.
Verified: `cargo test -p liberado-tui` (213/213, unchanged) and `cargo test -p liberado-main-agent`
(15/15, unchanged), clean `cargo clippy --all-targets` for both (the handful of remaining warnings
in each are pre-existing, in untouched production-code lines).

### Minor dedup

- `format_uptime` is duplicated verbatim between `liberado-commands` and `tui` — `tui` already depends
  on `liberado-commands` (for the shared slash-command dispatcher), so this is a one-line fix: import
  it instead of re-implementing it. (WebUI's own `format_uptime`-named function produces different
  output and is a coincidental name collision, not real duplication — leave it.)
- `crates/mcp/tests/factory.rs` and `crates/mcp/tests/runtime.rs` each define their own `McpHandler`
  test double (`EchoServer`/`TestServer`) with duplicated `read_resource`/`server_info`/`get_prompt`
  stub impls. `liberado-test-support` doesn't cover this (it's `ToolRuntime`/`RuntimeFactory` doubles,
  a different trait) — a small shared `McpHandler` test helper inside `mcp`'s own test tree would tidy
  this up. Low priority.
- `server/src/api.rs`'s `chat_stream_post`/`chat_stream_get` are near-identical 5-line handlers —
  worth a two-minute look to confirm the GET/POST split is load-bearing for `EventSource` clients
  (likely is) before deciding whether to share a common inner function.

## Not worth doing

- The other ~70 of `cargo dupes`' 75 flagged groups are trivial 2-4 line idiomatic Rust boilerplate an
  AST clone-detector always flags: `is_empty`/`len` pairs, builder `with_x` methods, tiny `new()`
  constructors, `Message::system`/`user`/`assistant` one-liners, `NoopRuntime`-style stub impls.
  Deduplicating these would hurt readability for no real maintenance benefit.
- WebUI's five `fetch_status`/`fetch_catalog`/`fetch_reactions`/`fetch_conversations`/`fetch_vault`
  functions look dedup-able (a generic `fetch_json<T>` helper) but differ enough in error-message
  shape that the generic version wouldn't clearly be simpler. Not worth it.
- A type-enforced "guards can only downgrade" invariant (e.g. a sealed `Downgraded` newtype only
  constructible inside `downgrade()`) — the current `Option<BlockReason>`-returning `match` is
  convention-enforced, not compiler-enforced, but the guard pipeline is small and well-tested. Worth
  revisiting only if the guard pipeline grows substantially.
- An `OrchestratorConfig`/builder struct to replace `Orchestrator::new`'s 6-argument constructor
  (currently `#[allow(clippy::too_many_arguments)]`) — real but low-urgency; construction happens in
  one or two places in `bootstrap`.
- `consequence_catalog: Vec<(String, Consequence)>` being stringly-keyed from `orchestrator` down into
  `RiskGatedToolRuntime`, where a lookup miss silently defaults to the most permissive `Consequence::ReadOnly`
  (`risk_gated.rs:129`) — worth at least a `tracing::warn!` on a miss so a typo'd MCP name in the
  catalog doesn't silently downgrade its risk rating with zero signal. Cheap to add whenever that file
  is touched next; not urgent enough to justify a standalone change today.

## Coupling — no real violations found

The documented bottom-up layering (`docs/spec/architecture/overview.md`'s crate map) holds: no lower crate
imports a higher one. Two minor, acceptable-as-is notes: `conversation-store` depends on `provider`
for `Message`/`Role` (the store needs LLM-message vocabulary to serialize; would be cleaner if those
types lived in `common`, but not worth moving today), and `config-loader` depends on `common` for
`Zone`/`Capability` to validate config (an unavoidable mild upward pull for a validator). `server`'s
10 workspace dependencies (the most of any crate) is exactly what's expected of a top-of-stack
assembler wiring `Daemon`/`ChatSessions`/`McpRegistry`/`Orchestrator` — not a smell.

## Test coverage — 81% lines overall, genuinely healthy

Per the user's explicit calibration during this audit: coverage gaps that would need complex mocking
to close (async orchestration loops, real HTTP handlers, tools that shell out to real processes) are
**not** worth chasing — this project's testing strategy deliberately pairs mocked unit tests for
deterministic logic with live/integration testing for everything else, and 80%+ overall is a good
number, not a floor to push toward 100%.

Consistent with that: `server/src/{api,lib,state}.rs` (0%, the HTTP/SSE API surface — verified by live
smoke runs per `crates/cli/ARCHITECTURE.md`), `heuristics-tuner/src/search.rs` (45% lines — the
`run_tuner`/`run_executor_tuner` async loops only have `#[ignore]`d live tests, by design), `mcp-forge/
src/build.rs` (0%, shells out to real `cargo install`), and `tui/src/render/*.rs` (0% across the
board — terminal rendering needs buffer-snapshot testing, a different and heavier kind of test
infrastructure than a mock) are all **accepted, not gaps to fix**.

One area that doesn't fall under that umbrella: `crates/config/src/lib.rs` is at 74% lines, and the
single largest uncovered block is `config_dir()` (lines 94-129) — the 4-tier config-directory
resolution order (env var → platform dir if populated → walk up from the binary → platform dir
fallback). This is genuinely important (get it wrong and the whole system silently can't find its
config) and doesn't need complex mocking to test — just `tempfile` dirs and env-var manipulation,
patterns already used elsewhere in this workspace's tests. Worth adding a few cases (each tier winning
when earlier ones are absent/empty) next time this file is touched; not urgent enough to justify a
standalone change today.

## What was NOT done this pass

This audit itself was a read-only survey, per the request — no code changes landed in the same pass.
The four items called out above (Priority 1's three fixes, plus the `common::config` split) were
picked up and fixed in follow-up work shortly after. Everything else in this document (the
`heuristics-tuner` module split, test extraction from oversized files, the minor dedups, the coverage
gaps) remains an open, documented backlog — safe to leave as-is, same posture as
`crate-modularity-audit.md`.
