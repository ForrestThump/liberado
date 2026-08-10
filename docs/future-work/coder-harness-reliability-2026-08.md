# Coding harness reliability — measurement and fixes, 2026-08

**Status**: Active. Fixes through PR #119 are on `main`. The measurement is real but incomplete —
read [Where the numbers came from](#where-the-numbers-came-from) before quoting it.

**Who this is for**: anyone picking up the coding pack. It records what was measured, what was
fixed, and — more usefully — **which plausible ideas were tried and did not work**, so the next
person does not spend a day rediscovering them.

---

## The one-line version

The coding pack could not finish a task that another harness finished on the same model, same repo
and same prompt. Every fix since has come from reading a failed run's trace, never from reasoning
about the code. Edit failure went from ~66% to 8%; the pack still has not been shown to complete an
unattended task end to end.

---

## Where the numbers came from

An A/B against **Kilo Code**, chosen because it is built on OpenCode and can be pointed at the same
model we use. Task F11, `deepseek/deepseek-v4-pro`, same repo, same commit, same prompt text.

| | Kilo Code | Liberado (before) | Liberado (2026-08-10) |
|---|---|---|---|
| Produced compiling code | yes | **no**, across 5 runs | n/a — different task |
| Reads per successful edit | 6.5 | 1.0 | 2.7 |
| Edit failure rate | — | 66–70% | **8%** (2 of 26) |

**The caveat that matters.** The last column is from a *different task* (P3.1a, ACP session store),
because F11 had been implemented by then and was no longer a fair test. A drop from 66% to 8% is
larger than task variation comfortably explains, but it is **not a controlled comparison** and
should not be reported as one. Re-running F11-equivalent work on a fresh harness is still open.

**Do not blame the model.** This was tried and was wrong. The same model, in another harness, on the
same task, did good work. Every failure since has had a harness cause.

---

## What was actually broken

Each row is a real defect found by reading a trace, not by review. The pattern is worth internalising:
**the guards were usually right that something was wrong, and wrong about the remedy.**

| PR | The failure | Why it was invisible |
|---|---|---|
| [#106](https://github.com/ForrestThump/liberado/pull/106) | `write_file` silently destroyed a file | Truncation looks identical to a successful write |
| [#107](https://github.com/ForrestThump/liberado/pull/107) | Edits died on CRLF and BOM; prompts were baked into the binary | The model's view of a file differed from the bytes on disk |
| [#108](https://github.com/ForrestThump/liberado/pull/108) | Hashline offered *alongside* raw-text edit tools | 14 of 41 anchors were contaminated by hashline tags |
| [#109](https://github.com/ForrestThump/liberado/pull/109) | An error message advertised the flag that bypassed it | The model took the advice, every time |
| [#110](https://github.com/ForrestThump/liberado/pull/110) | Reviewers ran on the coder's model, not the configured critic | Config shadow — the setting parsed and reached nobody |
| [#112](https://github.com/ForrestThump/liberado/pull/112) | Cold worktree, no build cache; first `cargo` call cost minutes | Looked like model slowness |
| [#113](https://github.com/ForrestThump/liberado/pull/113) | Documented TOML key `[coder.workspace]` was not the key serde read | Shipped *in the PR that added warm-up* |
| [#114](https://github.com/ForrestThump/liberado/pull/114) | `validate` answered `{"configured": false}` | The model asked the right question at turn 8 and got a shrug |
| [#115](https://github.com/ForrestThump/liberado/pull/115) | No `grep`; search was named something the model does not call | Model reached for a tool that did not exist |
| [#116](https://github.com/ForrestThump/liberado/pull/116) | Worktree registry race on Windows | Passed locally, failed only on the runner |
| [#117](https://github.com/ForrestThump/liberado/pull/117) | Traces recorded what the model *returned*, never what it was *sent* | The system prompt was unrecoverable from any run |
| [#118](https://github.com/ForrestThump/liberado/pull/118) | `git diff` shows tracked files only | A model's own new file was invisible to it and to the critic |
| [#119](https://github.com/ForrestThump/liberado/pull/119) | A full disk was classified as a code failure | Exit 101 is the same for a broken crate and a broken machine |

**#118 is the one to read if you only read one.** The model wrote a 334-line module, called
`git_diff` four times, was shown nothing each time, concluded *"the file doesn't exist yet — that's
the root cause of the build failure"*, and wrote the whole module again. Writing a file is the most
common first act of a coding task, and the tool that answers "what have I changed" could not see it.

---

## Failed hypotheses — read this before repeating them

These are the expensive part of the record. All three were reasonable; none worked.

### 1. "Give it `grep` and it will explore like Kilo does" — no

`grep` adoption worked as a *behaviour* change: calls went from 1 to 10 in a run. Reads per
successful edit went **1.0 → 3.0**, and the edit failure rate went **66% → 70%**. Offering a better
tool changed which tools were called and did not change the outcome.

### 2. "Give it `todowrite` and it will plan before editing" — no

Also adopted (7 calls in a run). Reads per successful edit **3.0 → 2.8**, failure rate unchanged.
Kilo's call order on the winning run was `read × 14 → todowrite → edit`, and the theory was that the
plan step is what creates the gap. Reproducing the *call* did not reproduce the *gap*.

**`todowrite` is not on `main`.** It lives on `exp/tuning-scratch` with the grep-ambiguity contexts
and prompt edits, held back because it has no evidence behind it. See [Open branches](#open-branches).

### 3. "Turn hash anchoring back on / build a better matcher" — no

A deep read of four reference harnesses says the opposite of what was expected:

| Harness | Edit matching strategy |
|---|---|
| OpenCode | strict exact match only |
| Kilo Code | strict exact match only |
| kimi-code | strict exact match only |
| oh-my-pi | a 10-rung fuzzy ladder |

**We already have a more capable matcher than Kilo and fail far more often.** Matching was never the
bottleneck. The mechanism behind Kilo's reliability is *call order* — it reads enough to write a
correct anchor the first time.

One rung of oh-my-pi's ladder was ported and then removed: mutation BQ deleted the strict ladder and
**all of its own tests still passed**, because `normalize_for_fuzzy` already trims and folds
typography. The ladder was redundant. That note lives in
[`crates/coder-tools/src/fuzzy_match.rs`](../../crates/coder-tools/src/fuzzy_match.rs).

---

## The recurring bug class: config shadows

**A config value that parses is not a config value that is read.** Ten instances have now shipped
green while a consumer hardcoded a literal instead: `[coder.gate]`, `[coder.coder]`,
`[coder.progress]`, `trace_dir`, the coder role model, two in `coder-runner/src/main.rs`,
`[coder.workspace]` (a serde key that did not exist), and the critic model in #110.

Symptom: changing the setting does nothing, silently. When you add a field to `CoderTuning`, grep
every `CoderRunConfig {` initializer and confirm yours arrives.
`crates/test-support/tests/config_literal_rules.rs` is the mechanical guard; extend it rather than
relying on care.

---

## How to debug a run

**Read the trace. Do not re-derive it.** Four consecutive failures were each diagnosed by reading
Rust and guessing, while the model's own explanation of the problem sat unread in a file.

Every run writes `<workspace>/coder-traces/<session>-attempt-N-<ts>.json`. The schema is
`{ session_id, request, events: [...] }`, one flat event list. Useful `type` values:

- `model_request_sent` — turn, tools offered, message count, system prompt hash, and (once per
  distinct hash) the prompt text. Added in #117; this is how you find out what the model was told.
- `model_turn_finished` — the model's text verbatim, and why the turn ended
- `tool_started` / `tool_finished` — what it called and whether the call failed
- `loop_guard_triggered`, `critic_verdict`, `validation_finished`

Counting reads per successful edit from a trace is a dozen lines of Python over `tool_finished`.
Treat `read_file | grep | list_files | list_symbols | git_diff | git_status` as reads and
`edit_file | write_file | apply_patch` as edits.

`[coder] trace_formats` can also emit `openai-messages` — the flat shape Kilo Code and OpenHands
persist — for comparing a run against another harness on the same task. Note that **Kilo's export
drops the system message**, which is exactly why #117 exists on our side.

---

## Operational gotchas

**Disk exhaustion is a live failure mode, and it does not announce itself.** One run died at 0.1 GB
free. Two causes, both recurring:

- `target/` reached **71.6 GB** in the main checkout. `cargo clean` is the fix and costs a rebuild.
- `cargo-mutants` copies the whole workspace into `%TEMP%` per run and **does not clean up when
  killed**. Eight leaked clones held ~23 GB. Sweep `%TEMP%\cargo-mutants-*`.

#119 makes the harness *report* this honestly instead of asking the model to fix it, but it does not
prevent it. Check free space before a long dispatch.

**Reinstall the bridge after merging anything the bridge links.** A dispatched run tests the
installed `liberado-acp.exe`, not the working tree. A run once silently tested a stale binary; it was
caught only by an error string in the trace that no longer existed in the source. Verify with a
string that only the new build contains.

---

## Open branches

| Branch | What is on it | Why it is not merged |
|---|---|---|
| `exp/tuning-scratch` | `todowrite`, grep ambiguity contexts, `prompts/coder/coder.md` edits | Measured **neutral to slightly worse**. Kept because the measurement is what is valuable, not the code. |
| `lib-18ca8a53fbbd54f4-20612` | A partial P3.1a implementation (`session_store.rs`, 334 lines) | Produced by the run that failed on disk + #118. Re-do against the fixed harness rather than salvage. |

Per the working rule agreed for this track: **tuning experiments live on one branch and become one
PR when performance is good.** Do not merge per iteration — the CI round trip is not worth it for a
change whose value is unmeasured.

---

## What is actually next

1. **Re-run a benchmark task on the fixed harness and measure.** The 8% number needs a second data
   point on a task nobody has implemented by hand. F8 and F11 are both contaminated now.
2. **P3.1a (durable ACP session records)** is a real open item and a decent benchmark: exploration-
   heavy, one new module plus wiring into a 1,700-line `main.rs`, compile-checkable, and it cannot
   break live Paseo because `loadSession` stays false. Brief shape: build the store only; do **not**
   flip the capability or make `session/load` succeed.
3. **Decide `exp/tuning-scratch`'s fate** with a measurement, not a preference.
4. **The completion gate (S1) is still default OFF** and unmeasured. It costs `1 + fresh_reviewers`
   model calls per attempt. S7 was supposed to measure it and has not.

---

## Rules earned the hard way

- **Run the mutation your test claims to catch**, and **confirm the mutation applied**. Two mutations
  in this work silently failed to match their needle. A no-op mutation is indistinguishable from an
  escape unless you assert on the substitution.
- **Never `git checkout <file>` in a mutation loop.** It wiped uncommitted work twice. Copy the file
  to a scratch directory and restore from there.
- **Run gates as their own step and read the output.** A `&&` chain once printed `failures:` and
  pushed in the same command.
- **A green suite does not prove the lockfile was committed.** `cargo metadata --locked` catches it
  in a second.
