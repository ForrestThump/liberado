# executor — Mutation Testing Report

**Date:** 2026-08-25
**Status:** historical
**Authority:** evidence
**Scope:** `liberado-executor`, full lib.

## Campaign history

| Ledger row | Survived | Caught | Viable |
|---|---:|---:|---:|
| markdown-era (`commit: null`) | 29 | 139 | 168 |
| `f6597d13` | 18 | 314 | 332 |
| `8622244` (fresh baseline) | **86** | 246 | 332 |
| `97fc00d` | 18 | 313 | 332 |
| `decd6e5` (final) | **12** | 319 | 332 |

The `f6597d13` row undercounted by ~5× on identical viability — trust fresh
numbers over any row whose generation you cannot reproduce.

## What was killed, by module

- **mvl.rs** — writer parent-dir creation, RFC3339 timestamps (parse + millis +
  Z), sha256 helpers against a known digest and key-order independence,
  message/catalog item rendering, execution sidecar path, terminal events,
  tools_changed firing only on real offer changes, full-vs-delta prompts
  (follow-ups labelled `delta` and carrying only new turns — the fixture must
  grow the conversation, or delta legitimately emits nothing), system-message
  hashing, tool start/result events in both logs.
- **budget.rs** — wall-clock boundary, limit chaining and counting, turn-cap
  adjustment preserving extra limits.
- **lib.rs pure helpers** — spill-label sanitisation, char-boundary truncation
  (the `/=` step variant hangs a test binary; hangs count as kills),
  prompt-builder wording pins, doom/cycle escalation rungs and the one-time
  recovery bonus, wrap-up reserve arithmetic (`turn + WRAP_UP_TURNS - 1`) and
  withdraw-except-finish.
- **lib.rs stateful** — request observation (system hash forwarded, absent
  prompt recorded as `None`), model override fidelity, all six mvl wrappers
  writing their event type through to the session files (turn mapping
  `saturating_sub(1)` pinned), read/write batch repeat counting by identity
  (name+arguments, excluding the current call), ok-flags in traces, short-cycle
  detection including period-3 walk order, and an exact-zero cosine for
  disjoint token sets (a broken idf produces NaN).
- **risk_gated.rs** — compact numeric permission-request ids, held-authority
  summary across MCP and both zone kinds, the undeclared-zone fail-safe (no
  grant → deferred, action never invoked), and its human-granted exception
  (held `Write` on an undeclared zone runs direct).

## Accepted survivors

| Location | Mutant | Why it stands |
|---|---|---|
| `lib.rs:116` | boundary walk `>` → `>=` | Position 0 is always a char boundary; the equal case exits anyway (confirmed empirically). |
| `lib.rs:283` | `semantic()` → `Default::default()` | `ArgMatch::default()` is already `Semantic`. |
| `lib.rs:795/796`, `:1695`×3 | logging gates | Gate `tracing` calls only. |
| `lib.rs:1911` | `&&` → `\|\|` before doom re-set | Both arms write the same literal; the overwrite is unobservable. |
| `lib.rs:2416/2420` | cycle-window arithmetic | Output is a sorted, deduped projection; overlapping windows yield identical sets for every input tried. |
| `lib.rs:2477` | empty-token guard `&&` → `\|\|` | The scalar branch and an empty-vector cosine agree (both 0.0) wherever they differ. |
| `mvl.rs:210` | `warn` → `()` | Logging-only. |
| `risk_gated.rs:188` | `authority_decision` → `()` | Logging-only. |
| `risk_gated.rs:403/409` | dead zone-class fallbacks | The write-capability check returns before the write-class match whenever the grant is missing, so both fallback forms are unreachable. |

Line-drift warning, learned twice here: a survivor name taken from an older
outcomes file can point at a different operator once files are edited. Re-read
the source line before writing a test against it.

`Executor::converse_stream`'s error-classification bang needs a streaming
provider harness and remains the one harness-blocked site.

## Harness notes

- Per-mutant verification scripts MUST take the crate name as a parameter; a
  hardcoded `-p` once verified zero mutations against the wrong crate's tests.
- Run mutation loops in the background writing per-mutant results to a log;
  foreground loops exceed shell timeouts and die mid-mutation, leaving applied
  mutants AND poisoned scratch backups. Restore material belongs in git
  (HEAD + the mod declaration), never only in /tmp copies that later runs may
  overwrite with mutated content.
- A hung mutant (infinite loop) is a kill; wrap the inner cargo invocation in
  `timeout` and treat expiry as failure of the suite.
