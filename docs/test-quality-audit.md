## Deduplication Analysis (2026-07-30)

`cargo dupes` found 244 exact-duplicate groups (6,351 lines) and 66 near-duplicate groups (1,729 lines) across the workspace.

### Approaches Evaluated

#### Option A: Move test doubles into `liberado-common`

**Pro:** Every crate already depends on common — zero new dev-deps required.
**Con:** Common has zero workspace deps today. Adding test doubles that need `liberado-executor`, `liberado-provider`, `liberado-notify`, or `liberado-messaging` creates a circular dependency (those crates depend on common).

**Verdict:** Infeasible for anything beyond common's own types (`SampleProposal`, `SampleMcpDescriptor`).

#### Option B: Keep the dedicated `liberado-test-support` crate (current approach)

**Pro:** Clean dependency graph. 6 crates already use it (orchestrator, daemon, server, telegram-approvals, dispatch-pack, test-support itself). Zero production code pollution.
**Con:** Every consuming crate must add the dev-dep manually. Orphan-rule friction when a local trait (`RebindableRuntime` in mcp) needs to be implemented on a test-support type.

**Verdict:** Best option available. The remaining test-code duplication (`NoopRuntime` in 5+ locations, `vault_descriptor` in 3, `NoopFactory` in 2) is all in test modules that would need local trait impls regardless of where the struct lives.

### Remaining Duplication in Hardened Crates

| Pattern | Copies | Why not consolidated |
|---------|:------:|---------------------|
| `NoopRuntime` + `impl ToolRuntime` | 5+ | Already in test-support; local copies exist where test-support isn't a dev-dep (mcp, main-agent) |
| `vault_descriptor` | 3 | Private test helper; extractable as `pub fn sample_vault_descriptor() -> McpDescriptor` in common (feasible, low priority) |
| `NoopFactory` | 2 | Semantically different: one returns `unreachable!()`, other returns `Err(...)` |
| `granted_tools`/`granted_mcps` | 1 | **Fixed** — consolidated via `matching_names` helper |
