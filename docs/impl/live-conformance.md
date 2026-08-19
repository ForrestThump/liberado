---
kind: runbook
status: active
authority: implementation
domain: conformance
canonical_for: live-conformance-operation
open_items: false
---

# Run the conformance suites

Liberado has two production-shaped conformance tiers and one optional model tier. The suites assert
ground-truth outcomes, not only that a request returned without error.

## Tier 1 — deterministic daemon paths

Tier 1 runs in CI with a mock provider and real session, store, grant, and API seams. Its L1–L11
cases live in `crates/server/src/t1_conformance.rs`; the daemon reaction-path L9 case lives in
`crates/daemon/src/tests.rs`.

Run the focused suites with:

```text
cargo test -p liberado-server t1_conformance
cargo test -p liberado-daemon l9_cron_event_becomes_joinable_dispatched_session
```

These tests cover plumbing and policy. They do not prove that a deployed daemon has the correct
configuration, credentials, mounts, or external connectivity.

## Tier 2 — optional model-in-the-loop checks

Tier 2 is deliberately optional and ignored by default. Use it only when a change requires model
behavior rather than deterministic transport or state-machine evidence. Do not replace Tier 1 or
Tier 3 assertions with a model judgment.

## Tier 3 — deployed-daemon paths

`liberado-conformance` talks to a running daemon over HTTP, prints one JSON result per path, writes a
vault report, and returns a non-zero exit code for a blocking failure.

The homelab configuration is
[`deploy/homelab/config/conformance.toml`](../../deploy/homelab/config/conformance.toml). From the
repository, run:

```text
cargo run --locked -p liberado-conformance -- \
  --config deploy/homelab/config/conformance.toml
```

An installed binary can use the same file:

```text
liberado-conformance --config /path/to/conformance.toml
```

The default set is P1a–P4. P5 is advisory. P6 uses two real-inference background turns. P7 restarts
the daemon and therefore requires an explicit restart command and a runner that survives the
restart. Select optional paths with `--path p5,p6,p7`. Use `--advisory-counts` only when an advisory
P5 failure must fail the run.

The path definitions and default-set policy live beside the implementation in
`crates/conformance/src/result.rs`. Configuration fields and P7 safety notes live in the example
configuration. Treat a skipped path as skipped, not passed.

## Evidence rule

For each path, assert an observable result such as persisted state, a joinable session, an emitted
artifact, or an explicit refusal. A count is advisory unless the exact count is the contract.

When a live run finds a defect, add or strengthen the deterministic seam test that should prevent
the same class from returning. Keep dated run results outside this runbook under `docs/validation/`
when they remain useful.
