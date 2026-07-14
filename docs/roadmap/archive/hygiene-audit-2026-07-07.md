# Hygiene audit + strategic reprioritization (2026-07-07)

Two things happened in one pass: (1) a read-through of the whole roadmap corpus
(`current.md`/`overview.md`/`positioning.md`/`modularity.md`, all three prior hygiene audits,
`crate-modularity-audit.md`, `mcp-forge-backlog.md`, `vs-hermes.md`) to answer "what's actually
highest-leverage next," and (2) a live re-verification of the codebase (an independent Explore-agent
pass, then hand-checked against the real source before writing anything down here — two of its
claims didn't survive that check and are recorded below as false positives, not findings, so they
aren't re-investigated next time).

## Already fixed this session (quick items, done before this doc was written)

1. Three cold-start docs (`overview.md`, `current.md`, `contributing/agents.md`) still described
   `provider-deepseek`/`provider-openrouter` as separate crates — collapsed into
   `provider-openai-compat` a while back. Fixed.
2. `RiskGatedToolRuntime::invoke`'s `consequence_catalog` lookup miss silently defaulted to
   `Consequence::ReadOnly` with zero signal — now logs a warning naming the MCP (a miss here
   specifically means the capability set and the consequence catalog have drifted, since step 1
   already confirmed the MCP is granted).
3. `format_uptime` was byte-identical in `tui` and `liberado-commands` — `tui` now re-exports the
   shared copy.
4. `mcp-forge-backlog.md` predicted "an agent self-schedules its own wake-ups" as a good `mcp-forge`
   candidate — noted that `liberado-wakeup-mcp` already built exactly that, this session.
5. `hygiene-audit-2026-07-04.md`'s open question about `common::model` (`ModelProfile`/`ModelRole`)
   having zero consumers — checked: `config-loader`'s `Config.topology.models`/`model_roles` is a
   real consumer, one hop removed from a direct grep via `liberado_config`'s re-export. Not dead
   weight.
6. `crates/config/src/lib.rs`'s `config_dir()` (the 4-tier env-var → platform-dir → walk-up →
   fallback resolution) had zero test coverage despite being flagged as important. Extracted the
   pure `resolve_config_dir(env_dir, platform_dir, exe_dir)` helper (env mutation races under
   `cargo test`'s parallelism, and `dirs::config_dir()` isn't mockable) and added 6 tests covering
   all four tiers plus the walk-up boundary.
7. `heuristics-tuner`'s dispatcher-tuning and executor/subagent-tuning code was flat in the same
   files (`search.rs`, `generation.rs`) — split into `tool_loop_search.rs`/`tool_loop_generation.rs`,
   matching the existing `tool_loop_scoring.rs`/`tool_scenarios.rs` naming convention.
8. `tui/app.rs` (2527 lines) and `main-agent/sessions.rs` (1125 lines) were each roughly half
   `#[cfg(test)]` module — extracted via `#[path = "..."]` into `app/tests.rs`/`sessions/tests.rs`
   (a pure file-layout change; still a private inline module with full `super::*` access, not a
   real integration-test directory, since both modules reach private internals).

All verified: full `cargo build --workspace` / `cargo test --workspace` clean, zero new clippy
warnings anywhere.

## Priority 1 — worth fixing soon

### A vault read failure is silently treated as "the file is now empty"

`crates/daemon/src/vault_source.rs`'s `build_event` (called from `Daemon::process_change`, the real
watch-loop entry point — not a theoretical path):

```rust
async fn build_event(vault: &Vault, rel_path: &Path) -> Event {
    let content = vault.read(rel_path).await.unwrap_or_default();
    let hash = Vault::content_hash(&content);
    ...
```

If the read fails (permissions, the file disappearing between `attribute()` and this call, a
transient I/O error), `content` becomes `""`, a real `VaultNoteChanged` event fires with a
correlation ID derived from the empty-content hash, and the rest of the pipeline (dispatcher,
executor) reasons about a phantom empty note as if that's genuinely the file's content — not "we
don't know what changed." Fix: propagate the read error (skip this change and log, matching
`poll_tick`-style patterns elsewhere in this codebase) rather than substituting a default.

## Priority 2 — real, worth doing when nearby

### Two lock-poisoning landmines, both low-probability but total-blast-radius

- `main-agent/src/sessions.rs:467`'s `session_lock`: `self.locks.lock().unwrap()` on a
  `std::sync::Mutex<HashMap<Ulid, ...>>`, called on **every turn**. If any thread ever panics while
  holding this lock, every future turn panics too — the session manager never recovers.
- `common/src/catalog.rs`'s `register`/`deregister`/`descriptors`/`get`/`is_empty`/`len` (6 sites):
  same pattern on the live `CapabilityCatalog`'s `RwLock`, read on every dispatch.

Neither is an imminent bug — both critical sections do only trivial, panic-resistant work (a
`HashMap` insert/remove, `Instant::now()`, an ignored channel send), so actual poisoning is unlikely.
But "unlikely" isn't "impossible," and the blast radius (the whole catalog or session manager wedged
until process restart) is total. `parking_lot`'s non-poisoning locks, or an explicit
`.unwrap_or_else(|poisoned| poisoned.into_inner())` recovery at each site, would remove the failure
mode entirely rather than just making it rare.

### `crates/executor/src/lib.rs` (2171 lines) is a decomposition candidate, not yet flagged elsewhere

Defines the `ToolRuntime`/`RuntimeFactory`/`ResourceLimit` traits, `Budget`/`Task`/`Executor`, the
loop-detection helpers (`is_doom_loop`/`detect_short_cycle`), and a large inline test mock, all in
one file. Not god-file-shaped in the sense of mixing unrelated concerns (it's all "the agent loop"),
but the traits, the `Executor` impl, and the independently-testable loop-detection logic could each
be their own file within the crate. Lower urgency than the Priority 1 item above; opportunistic.

## Checked and did *not* hold up (recorded so they aren't re-investigated)

- **`telegram-approvals` depending on `liberado-daemon` is *not* a layering inversion.** It's a
  `[dev-dependencies]` entry, used only by the crate's own `tests/live_smoke.rs` integration test to
  build a realistic end-to-end scenario — completely normal, not a production coupling edge. The
  2026-07-04 coupling audit's "no real violations found" stands.
- **`provider-openai-compat` does *not* have zero tests** — it has real inline tests
  (`deepseek_from_env_uses_environment_variables`, `openrouter_from_env_uses_environment_variables`,
  `generic_from_env_works_for_an_arbitrary_new_backend`, etc.).
- **The CLI's SSE dispatch not using `ChatEvent::from_sse_data`** is real (confirmed: `chat_client.rs`
  uses the shared `SseDecoder` for framing but hand-rolls its own `match event.event.as_str()`
  dispatch instead of the typed enum) — but it isn't new. `hygiene-audit-2026-07-05.md`'s P2.5
  resolution already names this exact gap as a deliberately-deferred, smaller follow-up.
- **The dispatcher's one `TODO`** (`crates/dispatcher/src/lib.rs:384`, the empty-seed `ExecuteDirect`
  proposal case) isn't a fresh finding — `current.md`/`overview.md` already name it as the one
  still-deferred fuzzy case in the proposal workflow.

## The strategic question: what's actually highest-leverage next

Beyond hygiene, the project owner asked for a leverage-sorted view across doc updates, tech debt,
anti-patterns, and new features — with an explicit question of whether continued polish is the
right call or a way to avoid diving into harder work. Answer, based on what this pass actually
found: **the hygiene discipline here is excellent, not a stall tactic.** Three audits in five days,
and nearly every flagged item — including two production-reachable panics and one bug where a failed
safety-critical write was silently reported as success — has a resolution with tests and a full
clean-workspace verification. This pass mostly turned up small leftovers and doc staleness, not new
rot, which is itself the evidence the habit is working.

Where this pass changes the picture:

- **`liberado-common`'s split (`crate-modularity-audit.md` item 3) stays correctly deferred.** The
  audit's own reasoning holds: the right module boundaries want more real reuse data first, and this
  session's `liberado-standalone-kit` extraction (a *separate*, non-life-os repo for
  `turbovault-client`/`frontmatter-note`/`openai-compat-client`) is itself a new data point worth
  weighing before choosing between "new internal crates" and "the same external pattern" for
  `common`'s genuinely portable pieces (`WriteProvenance`, `Outcome`/`Report`/`ToolCall`).
- **Phase 4 — the `ExecutionEnvironment` trait — is the one item that should stop being deferred.**
  Per `positioning.md`, the "Hermes is still ahead" caveat holds only until Liberado closes four
  named gaps: self-improvement (done, riggers), cron (done), subagents (mostly done), **execution
  environments (not started)**. `vs-hermes.md` already has a concrete recommended shape (`Local`/
  `Docker`/one serverless backend, session-keyed workspace state). Nothing about it needs another
  hygiene pass to de-risk first — it's fully scoped and it's the one remaining gap against the
  project's own stated competitive thesis.

## References

- Companion to [`hygiene-audit-2026-07-04.md`](hygiene-audit-2026-07-04.md) (three of its Priority 2
  items closed out above) and [`hygiene-audit-2026-07-05.md`](hygiene-audit-2026-07-05.md).
- [`crate-modularity-audit.md`](crate-modularity-audit.md) — item 3 (the `common` split) discussed
  above, still deferred.
- [`../architecture/positioning.md`](../../architecture/positioning.md) and
  [`../ideas/vs-hermes.md`](../../ideas/vs-hermes.md) — the competitive framing behind the Phase 4
  recommendation.
