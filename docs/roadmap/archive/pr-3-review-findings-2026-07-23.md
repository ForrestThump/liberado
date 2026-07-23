# PR #3 review findings (2026-07-23)

Saved so follow-up work does not lose the merge review of
[PR #3](https://github.com/ForrestThump/liberado/pull/3)
(*Architecture hardening: module splits, T1 suite, M1/M1b pooling, A4 dual-store, docs*).

**Base:** `dev/post-turbovault-merge` (not `main`)  
**Head at review:** `architecture-hardening` @ `f9bb269`  
**Reviewer verdict:** do **not** merge to `main`; merge into base only after the bugs below (or explicit acceptance) and real CI green.

CI failures on the PR were **infra** (private `turbomcp` `develop` checkout), not suite evidence of code regressions.

## Issues

### 1 — bug: idle TTL has no reaper (`crates/mcp/src/pool.rs`)

Idle TTL was only evaluated on the *next* `try_checkout` for that name. No background reaper.
After check-in, a pooled stdio/HTTP runtime stayed alive indefinitely if that MCP was never
acquired again — contradicting `McpPoolingTuning`'s "do not pin forever" intent. With pooling
default-on this was a silent resource regression for infrequently used peers.

**Fix intent:** reap expired slots on pool activity *and* via a background tick using the pool clock.

### 2 — bug: M1b `degraded` Instant never used (`crates/common/src/catalog.rs`)

`mark_degraded` stored `Instant::now()` but nothing read it. `routing_descriptors()` permanently
omitted degraded names with no half-open probe, so recovery required an accidental non-routing
acquire path.

**Fix intent:** half-open TTL — after `degraded_ttl`, re-include the peer in routing (expire the
degraded entry). Wire the stored Instant.

### 3 — suggestion: pooling defaults on (high blast radius)

`McpPoolingTuning::default` / `McpPoolSettings::default` set `enabled: true`. Acceptable once a
reaper exists; document escape hatch `tuning.mcp_pooling.enabled = false` and reaper behavior.

**Fix intent:** keep default-on with reaper + docs (reaper was the prerequisite).

### 4 — suggestion: `publish_healthy` on pool checkout (`crates/mcp/src/factory.rs`)

Pool hit called `mark_healthy` before any liveness probe, briefly re-surfacing dead peers in
`routing_descriptors`.

**Fix intent:** do not mark healthy on pool checkout; mark healthy on fresh successful connect and
on successful invoke (clears degraded only when present).

### 5 — suggestion: unbounded concurrent fresh connects (`crates/mcp/src/pool.rs`)

While a checkout is outstanding, concurrent acquires fall back to fresh connects with no cap.
Under parallel goals this can spawn many children; last check-in wins.

**Fix intent:** per-name semaphore (`max_in_flight_per_name`, wait timeout) held for checkout lifetime.

### 6 — nit: `mem::forget(TempDir)` in A4 test (`crates/session-store/tests/hub_dual_store.rs`)

Leaked temp dirs for process lifetime.

**Fix intent:** keep `TempDir` in scope for the duration of the test body.

## Merge notes

- Do not land this PR on `main` until intentional promotion of the post-TurboVault tip.
- Homelab redeploy and registry UX remained open product/ops items (not blockers for the code fixes above).
