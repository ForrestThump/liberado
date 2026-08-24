---
kind: validation
status: historical
authority: evidence
crate: acp-bridge
recorded_at: 2026-08-24
commit: 862224478106eb226334fbb3eb766e722dd3b7dc
survived: 25
viable: 354
caught: 321
timeout: 8
---

# Mutation testing report — acp-bridge

Four campaigns on one branch: **171 → 120 → 77 → 47 → 25 survivors** across six fix
batches plus Agent C/B integrations. Every kill was verified individually with the
per-mutant loop (apply, filtered test fails, restore from scratch copy) before the
recording campaign.

## Session of 2026-08-24 (47 → 25)

Nine kills, each verified against its hand-applied mutant:

| Mutant | Killed by |
|---|---|
| `handle_session_new` cwd filter `!` deleted | empty `cwd` string must fall back, not become the workspace |
| `commit_and_branch` body → `Ok(())` | non-repo workspace must fail, not fake isolation |
| `commit_and_branch` success-check `!` deleted | successful `checkout -b` must be reported as success |
| `remediate_if_needed` body → `None` | enabled + actionable findings still isolates a branch first |
| `remediate_if_needed` `\|\|` → `&&` | enabled + empty findings skips *before* isolation — no branch may appear |
| `remediate_if_needed` guard `!` deleted | disabled remediation must not touch the tree at all |
| `warm_workspace_if_configured` body → `Ok(())` | enabled warmup refuses a tree that does not compile |
| `warm_workspace_if_configured` flag check `!` deleted | disabled warmup never consults the tree (broken manifest proves it) |
| `PermissionBroker::bind_wire` body → `()` | a bound wire carries the request; the completed reply returns as a decision |

The remediation trio is killed by observing **branch existence** after the call —
the mock provider runs dry immediately, so no executor round is needed; isolation
itself is the observable.

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

### Integration-heavy pack orchestration (10)

`run_coding_round` prior-feedback guard and its three findings-field deletes, the
goal/face/converse prompt drivers (`run_goal_prompt`, `finish_coding_run`,
`finish_coding_tail`, `run_face_prompt`), cancel_and_preserve,
permission_attach, ensure_converse reuse guard. Killing these needs a full
coding-pack round harness (worktree + scripted provider + event stream);
deferred as a cohort rather than chased one-by-one. The remediate/warm guards
and commit_and_branch left this cohort in the 2026-08-24 session.

### Env-dependent provider resolution (5)

build_provider env-key filter `!`, topology-profile selection flips (body→None
and `==`→`!=`) and the env-profile scan ordering (`==`→`!=`, `!` deleted) — all
require multi-environment fixtures around `LIBERADO_CONFIG_DIR`/key envs;
feasible but brittle, deferred. The sixth current provider survivor
(`fetch_live_models` empty-guard → true) is listed with the equivalents above.

## Anomalies noted for the next pass

Resolved in the 2026-08-24 session: the three anomalies below were re-checked on
a fresh campaign — the notification-routing pair and the step_from_json pair no
longer generate as survivors under the restored sources.

- `dispatch_stdin_message` is_notification `||`→`&&` survives despite notification
  routing being covered — needs a through-dispatch notification test (direct
  `dispatch_notification` calls do not exercise the branch).
- `dispatch_notification` sid match `==`→`!=` survives although the abort test
  asserts closure after a yield — worth re-checking under the recording runner.
- `coding_run::step_from_json` `||`→`&&` survives although direct unit tests pin
  both half-defined shapes — verify against a fresh tree before re-chasing.

## Campaign hygiene note

One crashed in-place campaign (exit on a Windows file-lock error) left its
applied mutant un-restored in `permission.rs`; the next run then generated
mutants from the mutated body and produced a skewed row (27 survivors over 292
viable). The clean regeneration at the same commit produced 354 viable / 25
survived and is the authoritative row. After any abnormal cargo-mutants exit:
`git diff` the crate before trusting or recording anything.
