# main-agent — Mutation Testing Report

**status:** historical
**authority:** evidence

**Update — 2026-08-23** (ledger campaigns at `0e14ecc` baseline → `9f44692` closing,
branch `fix/main-agent-mutant-survivors`)

| Metric | Baseline | After |
|--------|:---:|:---:|
| Viable mutants | 364 | 364 |
| Caught (+timeouts) | 140 (+3) | 190 (+3) |
| **Missed** | **56** | **6** |
| Unviable | 165 | 165 |

## Fixed (50 mutants, every one verified KILLED before recording)

Per repo rule, each mutation was applied by hand, the targeted new test had to fail,
and the source restored from a scratch copy before the next step.

- **dispatch_journal.rs (9):** data-dir resolution (`LIBERADO_DATA_DIR` + `.liberado`
  fallback), journal path shape, append writes exactly one newline-terminated JSONL line,
  inverted/lying parent-guard both abandon correctly on the success path, start/disposition
  record fields, display path hint.
- **compaction.rs (3):** exact token arithmetic — content + tool-call name + arguments JSON +
  result id. Relative assertions ("with call > without") survived every arithmetic swap; only
  an exact `estimate_tokens == 4` fixture pinned the formula.
- **face.rs (1):** D-e AskHuman strip on `DispatchBridge::delegate` — asserted through the hub
  session record's grant (kept `ExecuteMcp`, dropped `AskHuman`) plus the compact-report shape.
- **lib.rs (5):** `resume_stream` appends the answer as a keyed tool result and completes;
  `is_empty` both directions; `Rollback::drop` truncation on an *actually polled* future (the
  old cancellation test never polled, so neither arm ran — vacuous); `transient` accounting in
  `apply_available_tools` via exact `turn_tail` slicing.
- **sessions.rs (32):** `RunningTurn::publish` replay+broadcast split; delegation-mode prompt
  swap conjunction; streaming face-turn keeps the face prompt (inverted `!face_agent` guard);
  `uses_face_agent` requires both halves; named-profile model pinning; attach/in-flight
  bookkeeping (count, sessions, running, attach); detached `spawn_turn` runs and retires its
  entry; risk-gate arming matrix (each of live-catalog/consequences/zones sufficient alone);
  tool-only grants reach the scoped runtime (not NoTools); pre-turn dispatch grant ceiling
  (AskHuman stripped, rest kept); narrowing filters qualified-tool grants by parent MCP
  (arm-delete and `==`→`!=` both leak foreign tools); incoming user message counts toward the
  compaction trigger (`+`→`-` swap distinguished by a trigger sized to `estimate − 1`);
  `tail_after_user` drops only a leading user message; PassThrough forwards catalog+invoke;
  NoToolsRuntime refuses with its specific error.

New tests follow the crate's `#[path]` sibling convention (`dispatch_journal/tests.rs`,
`compaction/tests.rs`, `face/tests.rs`, `lib_tests.rs`, plus a survivor section in
`sessions/tests.rs`). `LIBERADO_DATA_DIR` mutation is serialized behind a lock (tokio mutex in
async tests — std guards across `.await` are unsound and clippy-flagged).

## Accepted survivors (6)

| Location | Mutant | Why accepted |
|---|---|---|
| `face.rs:110`, `sessions.rs:1595` | delete `overrides: Value::Null` from `SessionGrant { .. }` | `..Default::default()` supplies `Value::Null` — the literal equals the default. Equivalent by construction (checked against `SessionGrant`'s derived `Default`). |
| `sessions.rs:1904` | `>` → `>=` in `maybe_compact` | Gates only a `tracing::error!` emission when `tail_persist_failures > 0`. No state change either way. |
| `sessions.rs:2023:20` | guard `c > 0` → `true` | A marker at index 0 is impossible: the root node is authored `System`, never `COMPACTION_AUTHOR` (module doc states the invariant). `Some(c)` implies `c >= 1`. |
| `sessions.rs:2023:22` | `>` → `>=` in `elide_before_latest_marker` | Same invariant: `c >= 0` is always true where the pattern matched. |
| `sessions.rs:2067` | `NoToolsRuntime::catalog` body → `vec![]` | The original body *is* `Vec::new()`. Identical expression. |

Plus one near-equivalent kept from the baseline list: `dispatch_journal.rs:57` `ensure_parent`
→ `true` differs only in *which warning* is logged when `create_dir_all` fails; the open attempt
afterwards fails identically and nothing is written in either case (the failure-path twin,
→ `false`, is killed by the happy-path append test).

The three baseline `timeout` entries (`lib.rs:326`, `sessions.rs:1140`, `sessions.rs:1191`) now
hang the suite under mutation instead of passing it — each was separately verified KILLED by a
targeted test; cargo-mutants records the run as `timeout`, which is caught-class, not survived.

## Process notes

- Two vacuous pre-existing tests were found by this campaign: the rollback-drop test (never
  polled its future) and the missing streaming-face-prompt coverage. Both replaced/strengthened.
- Simulating a body-replacement mutant with an unbalanced brace produces a compile error that
  looks like a kill; every "killed" verdict above re-checked with a compiling mutant.
