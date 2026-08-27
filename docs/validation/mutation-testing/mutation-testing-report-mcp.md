---
kind: validation
status: historical
authority: evidence
crate: mcp
recorded_at: 2026-08-25
commit: 5ca11c681b068ca5ce7e1602f99f18ef9310dc01
survived: 5
viable: 100
caught: 86
timeout: 0
---

# Mutation testing report — mcp

Campaign at the mutants-integration branch tip: **34 → 14 survivors** across two
fix batches (pool lifecycle, live-registry cache, factory health publishing).
Every kill was verified individually with the per-mutant loop (apply, filtered
test fails, restore from scratch copy) before the recording campaign.

## Killed this campaign

- **pool.rs**: reap/take_expired disabled-pool gating and body deletion;
  checkout TTL boundary (`idle > ttl`, exact-boundary pinned on a controlled
  clock); `invalidate` slot removal; success-with-stale-dead-flag must NOT take
  the discard path; dead-connection must not re-enter the pool;
  `AsToolRuntime`/`PermittedRuntime` delegation verbatim.
- **live_runtime.rs**: the peer-name cache must actually cache - an unchanged
  registry reconnects zero peers on subsequent catalog/invoke calls (kills all
  three wrong `sorted_names` bodies and the inverted equality guard); empty
  registry stays honestly empty.
- **factory.rs**: a successful connect publishes healthy again after a
  pre-degraded mark.
- **lib.rs**: `to_tool_def` fallback schema; `arguments_to_map` shapes.

## Accepted residues

### Transport-bound `TurbomcpRuntime` impl (4)

`rebind_provenance -> ()`, `connection_is_dead -> true/false`, `shutdown -> ()`.
These sit behind `turbomcp_client::Client<T>`: reaching them in-process needs a
fake `turbomcp_transport_traits::Transport` (~10 `Pin<Box<dyn Future>>` methods)
plus the initialize/`list_tools` protocol round-trip. The behaviours are
exercised end-to-end by the stdio smoke path instead; the pool-level contract
that *consumes* them (`connection_is_dead` gating discard) is fully pinned by
the pool survivor tests with a controllable double.

### Log-only guards (1)

`refresh_sync`'s `if !failed.is_empty()` warn gate - both arms leave the same
returned runtime; flipping it changes only which log line fires.
