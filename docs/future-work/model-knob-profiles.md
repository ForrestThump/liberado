# Per-model knob profiles and the tuning ledger

**Status**: Design, recorded 2026-08-11. Not scheduled, deliberately. We are tuning against
`deepseek-v4-pro` and `deepseek-v4-flash` and that is the right focus right now. This exists so the
current tuning does not quietly harden into assumptions that only hold for one model.

**Owner's framing:** *"The harness–model pairing matters more than the harness, or the model."*

---

## The claim, and why it changes the design

An optimum found for DeepSeek v4 is not an optimum for Kimi K3 or Grok 4.5. Some models benefit from
hash-anchored edits like oh-my-pi's; some are hurt by them. Some need a nudge before a guard
escalates; some ignore nudges entirely — we already measured that one: *"a nudge alone did not
change DeepSeek/Gemini's behaviour in live testing — they repeated anyway"*, which is why the
escalation ladder removes tools rather than asking twice.

That single line is the whole argument. It is a **per-model finding baked into shared code as a
constant.** If Kimi K3 responds to nudges, we have hardcoded a worse harness for it and will never
notice, because nothing records which model a failure came from alongside which settings were in
force.

Three things follow:

1. **Every behavioural constant should be a knob**, changeable without recompiling.
2. **Each model gets a profile** — a named set of knob values, applied automatically when that model
   is selected.
3. **Every run is recorded** with its model, its knobs, its task and its outcome, so profiles are
   derived from evidence instead of taste.

---

## 1. Knobs, and the standard they have to meet

A knob is only a knob if changing it changes behaviour **without a rebuild**. We have shipped ten
settings that parsed, validated and reached nobody because a consumer hardcoded a literal — the
config-shadow class in `CLAUDE.md`. That failure mode is an irritation today and would be fatal to
this system, because a profile that silently does not apply produces measurements that look valid
and describe nothing.

`crates/test-support/tests/config_literal_rules.rs` is the existing mechanical guard, and it is
narrower than its name suggests: its `BANNED` list holds **one entry** — `HashlineConfig {` — checked
across two surfaces (`coder-runner`, `acp-bridge`). Nine of the ten known config shadows are outside
what it can see.

**It has to grow to cover every knob before profiles are worth building**, and growing it is cheap:
one line per config type. Do that first, because a profile that silently fails to apply is worse
than no profile — it produces measurements that look valid and describe nothing.

Candidates already in the code, most of them currently constants:

| Area | Knob | Today |
|---|---|---|
| Loop guards | doom-loop threshold; arg-match mode per tool; escalation ladder (nudge → remove → refuse); whether a withdrawn tool is ever restored | constants + `LoopProfile` |
| Edits | fuzzy matching on/off; similarity threshold; hashline on/off; `replace_all` policy; ambiguity behaviour | partly `[coder.edit]` |
| Context | compaction trigger; tail size; offload threshold; truncation caps | partly config |
| Turns | max turns; recovery top-up size; reserve turns | partly config |
| Review | gate on/off; reviewer count; critic model | `[coder.gate]` |
| Prompts | every role prompt | `prompts/` (already file-based, PR #107) |

Sources of further knob ideas, deliberately: Grok Build, Kimi Code, OpenCode, oh-my-pi, and the
three studied in [`harness-study-2026-08.md`](harness-study-2026-08.md). Reverse-engineering a proven harness is
cheaper than discovering the same knob by trial.

## 2. Profiles

```toml
[models."deepseek/deepseek-v4-pro"]
extends       = "deepseek-family"
edit.fuzzy    = true
loop.arg_match_default = "semantic"

[models."deepseek/deepseek-v4-flash"]
extends       = "deepseek-family"
gate.enabled  = false          # cheap model, reviewer cost dominates

[models."x-ai/grok-5"]
extends       = "x-ai/grok-4.5" # a new model starts from its nearest relative
```

`extends` is the important part. A new release is not a blank slate: **start from the closest known
profile and make small informed adjustments.** That is the difference between a tractable search and
an intractable one.

An unknown model gets documented defaults and a warning in the run record that it is unprofiled, so
its results are never mistaken for tuned ones.

## 3. The ledger

A SQLite database under `<LIBERADO_DATA_DIR>/`, one row per run:

- model, profile name, and the **resolved knob values actually in force** (not the profile's name —
  the values, because `extends` and overrides make the name insufficient)
- task id and description, repo commit
- outcome, and on failure the classified failure mode plus the trace path
- tokens in/out/cached, wall clock, turns used
- edits, edit failures, tools withdrawn

Two properties matter more than the schema. **Resolved values, not references** — otherwise a
profile edit silently rewrites history. And **a pointer to the trace, not a copy** — the trace is
already the record of what happened, and duplicating it invites the two to disagree.

## 4. How tuning actually happens

Not brute force. The combinatorics are hopeless and each sample costs a real model call.

The loop is: **run, record, read the failures, form a hypothesis, change one knob, measure.** The
ledger's job is to make step three cheap — an agent reads the failed runs for a given model, groups
them by failure mode, and proposes which knob to move and why. That is a local search seeded by
evidence, which is the same method that produced every fix in the reliability work so far. The
difference is that today the evidence lives in my head and in PR bodies; the ledger puts it
somewhere a fresh agent can query.

**The honest caveat:** with one or two runs per configuration, most differences will not be
significant. The ledger should record enough to *notice* a pattern and prompt a closer look; it
should not be read as an A/B test with n=1. Anything that reports a winner from a single run is
lying, and the reads-per-edit mistake in
[`coder-harness-reliability-2026-08.md`](coder-harness-reliability-2026-08.md) is the cautionary
example: a plausible metric, over-read from too few samples, that pointed at the wrong lever for
weeks.

---

## Why this is not being built now

The foundation is not ready, and building the ledger first would produce a well-instrumented record
of a harness that still has known bugs.

Prerequisites, in order:

1. **The open harness bugs close.** A ledger that records runs failed by a defective guard measures
   the guard.
2. **The knobs exist and are proven to be read.** Extend `config_literal_rules.rs` first.
3. **The event contract firms up** (see [`harness-study-2026-08.md`](harness-study-2026-08.md) §8), so the ledger
   can ingest runs from forked harnesses too, not only our own.

Then this is worth building, and not before. The value is entirely in the accumulated record, so
starting it against a moving foundation wastes the accumulation.
