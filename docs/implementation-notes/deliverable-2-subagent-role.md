# Deliverable #2 — subagent/direct journal split

Branch: `feat/journal-subagent-role`

## What was done

**Step 1 — `crates/provider/src/latency.rs`**  
Added `AgentRole::Subagent` variant with `as_str()` returning `"subagent"`. Fixed the `MeteredProvider` doc comment, which had claimed both role and correlation came from task-local scope — only correlation is task-local; role is bound at construction (the deliverable cited `latency.rs:47` as the `CORRELATION` task-local).

**Step 2 — `crates/bootstrap/src/lib.rs`**  
New `RoleProviders.subagent_worker` field: same ModelRole::Subagent backend, tagged `AgentRole::Subagent`. Both `configure_daemon` and `build_dispatch_pack` pass it to `OrchestratorInfra` via `.with_subagent_provider(...)`. The existing `providers.subagent` (tagged Orchestrator) is unchanged.

**Step 3 — `crates/orchestrator/src/lib.rs`**  
`Orchestrator` and `OrchestratorInfra` each gained a `subagent_provider` field defaulting to their own provider (zero churn at all 55 existing `Orchestrator::new` call sites — no tests had to change). Three sites switched to the subagent provider:
- `run`'s `DispatchSubagent` arm (~line 997)
- `execute_approved`'s `ProposedAction::Subagent` arm (~line 1236)
- `dispatch_parallel` (the `tokio::spawn` worker, ~line 1306)

Both `ExecuteDirect` arms still use `self.provider` (tagged Orchestrator).

**Step 4 — cost reader**  
`journal_shape.rs`: added a `subagent` role round-trip test and a back-compat fixture — a pre-change `orchestrator` record (with an unknown `future_field`) still parses and keeps its label. The `liberado-cost` tool renders `subagent` on its own row automatically (rollup groups by `role` string dynamically).

**Tests**
- `subagent_and_direct_execution_journal_distinct_roles` — one fixture asserts both roles: `ExecuteDirect` journals `"orchestrator"` while `dispatch_parallel` (spawn case) journals `"subagent"`. Cron/vault triggers reach the orchestrator through these same arms, so they are implicitly covered.
- The existing daemon test `approved_subagent_execution_is_attributed_to_the_proposal_correlation` was extended to also assert every event's `role == "subagent"` (covers approval path line 1205).

**Gates (all clean)**
- `cargo fmt --all --check` — clean
- `cargo test --workspace` — all pass
- `cargo clippy --workspace --all-targets` — only a pre-existing `liberado-webui` warning (untouched)

## Caveats

### No real latency journal data exists locally
`.liberado/` has conversations, dispatches, goal-sessions, logs, sessions, and tuner — but **no `latency/events.jsonl`**. The acceptance item "run it against real data and put the numbers in the PR" cannot be satisfied. `cargo run -p liberado-cost -- --data-dir .liberado` reports `events: 0`.

A synthetic fixture confirmed `liberado-cost` renders `face`, `orchestrator`, and `subagent` on three separate rows.

### Historical `orchestrator` records cannot be split
The journal is append-only and history is never rewritten. Every `orchestrator` entry written before this change contains both direct execution and delegation work perma-mixed. The new `subagent` label only appears on calls recorded after this change lands. The PR should state this plainly.
