---
status: historical
authority: evidence
crate: acp-bridge
recorded_at: 2026-08-23
commit: d2538ddd58873feda983ae5d95894d0f9dd5df97
survived: 47
viable: 454
caught: 393
timeout: 14
---

# Mutation testing report — acp-bridge

Three campaigns on one branch: **171 → 120 → 77 → 47 survivors** across five fix
batches plus Agent C/B integrations. Every kill was verified individually with the
per-mutant loop (apply, filtered test fails, restore from scratch copy) before the
recording campaign.

## Accepted residues, by class

### Equivalent mutants — unkillable by any test (8)

| Location | Why |
|---|---|
| `main.rs` NoTools::catalog → vec![] | Body already returns an empty vec |
| `main.rs` snapshot_turns Assistant arm | Deleted arm maps to the same `"assistant"` fallback |
| `main.rs` extract_prompt text / resource_link arms | Fall-through unknown-type arm renders identically |
| `permission.rs` OPT_DENY arm deletion | Falls to `_ => Deny` — identical decision |
| `provider.rs` openrouter fallback arm | Identical list lives in the `_` arm |
| `provider.rs` fetch_live_models guard→true | Empty ids take either path into the same fallback |

### Win32 handle code — not exercisable in-process (6)

`stdin_guard.rs` — the wire-detach ritual duplicates handles and swaps the process
stdin at raw Win32 level. Guarded by its doc-comment invariants and the stdio smoke
test instead.

### IO-bound stdout writers (3)

`wire.rs` write_rpc_request / emit and `main.rs` report_config_dir write to real
stdout or emit log lines only; asserting them requires capturing stdout the process
owns.

### Integration-heavy pack orchestration (17)

`run_coding_round` struct-field deletes, remediate/warm guards,
commit_and_branch, the goal/face/converse prompt drivers, cancel_and_preserve,
finish_coding_tail, permission_attach, ensure_converse reuse guard. Killing these
needs a full coding-pack round harness (worktree + scripted provider + event
stream); deferred as a cohort rather than chased one-by-one.

### Env-dependent provider resolution (5)

build_provider model-override filter, topology-profile selection flips and the
env-key scan ordering — all require multi-environment fixtures around
`LIBERADO_CONFIG_DIR`/key envs; feasible but brittle, deferred.

## Anomalies noted for the next pass

- `dispatch_stdin_message` is_notification `||`→`&&` survives despite notification
  routing being covered — needs a through-dispatch notification test (direct
  `dispatch_notification` calls do not exercise the branch).
- `dispatch_notification` sid match `==`→`!=` survives although the abort test
  asserts closure after a yield — worth re-checking under the recording runner.
- `coding_run::step_from_json` `||`→`&&` survives although direct unit tests pin
  both half-defined shapes — verify against a fresh tree before re-chasing.
