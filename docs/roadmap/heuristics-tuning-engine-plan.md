# Heuristics Tuning Engine — Plan

**Status**: v1 built and used for a real tuning run 2026-07-03 (dispatcher layer; see the build
order below for what shipped). Captured here so the design survives before Phase 3 work begins —
the intent is to harden the tool-use/dispatch architecture by automatically finding its weak
points, rather than finding them slowly through dogfooding.

**Framing, confirmed after the first real run (2026-07-03)**: this is not a benchmark-chasing
exercise — a candidate that "wins" against the fixed scenario set isn't the point, and the first
real run proved exactly why (see "Scenario generation" below): it's a **maintenance tool that
keeps the dispatcher's prompt honest as the tool surface and data change**. When a new MCP is
added, or an existing one's shape changes, the dispatcher's prompt should be able to re-adjust
toward using it correctly in the situations that call for it — automatically, not by a human
noticing a misroute weeks later — while the existing hand-curated scenarios keep acting as a
regression net so the re-tune doesn't quietly break what already worked.

## Motivation

`liberado-eval` already runs a manual version of this loop — its own doc comment says it plainly:
"The loop is: run → read the misses → tune `SYSTEM_PROMPT` / tunables → run again." A human does
that by hand today. This plan automates it: generate diverse test goals, run them against a real
system entry point, score the results, propose targeted prompt tweaks, and iterate — surfacing
weak points in routing and tool-use before they show up in real dogfooding.
[`liberado-testing-and-eval-spec.md`](../specs/liberado-testing-and-eval-spec.md) §4.2 ("Real-model
eval suite") and §5 ("Logging Is the Fixture Pipeline") already describe the scoring dimensions and
the traced-run → fixture pipeline this plan builds on rather than replaces.

## Goals

- Automatically discover prompts/scenarios where the dispatcher, executor, or main agent
  misbehaves — wrong routing, unsafe acts, wasted tool calls, budget exhaustion — without a human
  hand-writing every scenario.
- **Re-adapt to a changing tool surface, not just fix wording.** When a tool is added, removed, or
  reshaped, the dispatcher's prompt should catch up to it — using the new tool correctly where it
  applies — without a human noticing the gap through dogfooding first. This is the actual point;
  scoring higher against a fixed scenario set is a proxy for it, not the goal itself (see
  "Scenario generation" below for the concrete mechanism this implies).
- Propose specific, evidenced prompt tweaks a human can review and merge — never auto-apply.
- Periodically step back from prompt-level tuning and have a model critique the *architecture*
  itself, not just wording — a second, broader mode alongside the tight local-search loop.
- Stay cheap and boundable: a human sets the call budget per tuning session before it runs.

## Non-goals (v1)

- No runtime/agent-writable config (Decision 14 stands — tuning proposes, it does not merge itself).
- No tuning against real production MCPs/vault by default — mocked tool runtimes first (see
  "Scoring During Tool-Loop Execution" below).
- No attempt to out-perform hand-curated `liberado-eval` scenarios on day one — the tuner
  *extends* eval's scoring, it doesn't replace the deterministic regression gate.

## Design

### New crate: `liberado-heuristics-tuner`

A new crate, not an extension of `liberado-eval` — eval's scenarios are hand-written `&'static`
Rust structs (compile-time, fixed); the tuner needs scenarios generated at runtime. But the tuner
should *depend on* eval's scoring shape rather than duplicate it. Concretely: `liberado-eval` is
currently bin-only (`crates/eval/Cargo.toml` has a `[[bin]]` target and no `[lib]`, so
`scenarios.rs`'s `Scenario`/`ExpectKind` types aren't reachable from other crates) — giving it a
thin `[lib]` target (or lifting `Scenario`/`ExpectKind` into `liberado-common` or a small shared
crate) is a prerequisite step, not an afterthought.

### Provider: OpenRouter backend

A new `liberado-provider-openrouter` crate implementing the existing `Provider` trait
(`liberado-provider`, Decision 13) — same shape as `liberado-provider-deepseek`. This is what
avoids rate-limiting when running many scenario/variant evaluations concurrently, and it slots into
existing architecture cleanly: nothing about the tuner needs to know it's talking to OpenRouter
specifically, since every LLM call in the system already goes through `Provider`.

### Scenario generation

- **v1 (shipped 2026-07-03)**: cold-prompt an LLM to generate candidate *prompts*, but scoring
  itself reuses `liberado-eval`'s fixed, hand-written 19 scenarios as-is — no dynamic scenario
  generation yet. This is a real, known gap, not an oversight: the first real tuning run (see
  `Dreams`-adjacent review, 2026-07-03) confirmed the fixed scenario set only ever exercises the
  MCPs someone thought to hand-write a case for at the time. Add a new MCP tomorrow and the tuner
  has no way to know it exists — it will keep scoring the same 19 goals forever, none of which
  touch the new tool. That's fine for "does prompt-tuning work at all" (v1's actual goal), but it
  doesn't yet deliver the real goal above (re-adapting to a changing tool surface).
- **Next, not yet built — topology-driven generation**: read the *live* `CapabilityCatalog` (the
  same one `liberado-dispatcher`/the daemon already build from `topology.toml` — no new source of
  truth, just a new consumer of an existing one) and synthesize plausible goals that would need
  each tool, weighted toward newly-added or reshaped ones. Fold these into the scoring pool
  *alongside* the fixed `liberado-eval` scenarios on every run — new tools get exercised, and the
  hand-curated 19 keep acting as the regression net so a re-tune triggered by a new tool can't
  quietly break already-working routing. This is the mechanism that actually delivers "add a tool,
  the engine adjusts automatically" rather than "the engine gets slightly better at 19 fixed
  goals forever."
- **Later still** (once there's real usage to learn from): mine `liberado-conversation-store`'s
  history (Liberado's own chat logs — "user post history" means our own dogfooded history, not
  anything external) for realistic goal shapes, as a third scenario source alongside the fixed set
  and the topology-driven one. Today there's ~none of this; the plan doesn't depend on it existing.
  This is the same traced-run → fixture idea `testing-and-eval-spec.md` §5 already describes for
  the manual eval, just automated.

### Search strategy

Two nested loops:

**Outer loop — cycle through layers.** Dispatcher, executor, main-agent each have their own
system prompt and their own failure shapes, and they're confounding variables with each other
(tuning the dispatcher changes what the executor downstream ever sees). Rather than fully
converging one layer before touching the next, round-robin across layers: tune each one for a
bounded number of generations, move to the next, and after a full cycle through all layers, loop
back around for another pass. **v1 starts with the dispatcher only** — it's the safety-critical
layer, and `liberado-eval`'s existing scoring targets it directly with no tool execution required
(cheapest possible loop to get working end-to-end). Executor and main-agent tuning are follow-up
cycles once the dispatcher loop is proven.

**Inner loop — local search within a layer, with restarts against local maxima.** Not pure greedy
hill-climbing (keep only the single best, tweak it, repeat — prone to getting stuck near wherever
the first candidate happened to land). Each generation's candidate pool combines:
- A small beam of mutations from the current best (the "5 tweaks" shape originally described).
- At least one **fresh, cold-started candidate** — prompt an LLM for a prompt from scratch for the
  use case, independent of the current best, then mutate that too. This is a Monte Carlo restart:
  it gives the search a chance to escape whatever basin the first candidate landed in, rather than
  only ever refining it.

Score every candidate in the pool concurrently (this is what the OpenRouter backend buys), keep the
top-K (a small beam, not just top-1), and iterate for a human-set number of generations before
moving to the next layer in the outer loop.

### Scoring during tool-loop execution

Reuse `liberado-eval`'s scoring dimensions (routing accuracy, safe-default rate, and the hard
UNSAFE-acts-must-stay-0 gate) for the dispatcher layer — no execution needed, scoring is purely
"did it pick the right `DispatchAction`." Once tuning extends to the executor/main-agent layers,
where actual tool calls happen: start with a **mocked `ToolRuntime`** (deterministic, fast, zero
side-effect risk — the same recording-mock pattern `liberado-test-support` already provides for
regular tests) rather than real sandboxed MCPs. Real sandboxed execution (mirroring the "live smoke
recipe" pattern already used for manual smoke tests) is a later, opt-in extension once the mocked
loop has proven useful — not a v1 requirement.

### Output: proposed diff + rubric, never auto-merged

Mirrors the riggers/`code-dispatch` self-improvement pattern from Phase 2 (plain-English task →
draft PR → human approval → publish, never auto-merges) — the same trust boundary applies here for
the same reason (Decision 14: agents don't write config/prompts).

The winning candidate doesn't get hot-written into the running system prompt. It's written as a
proposal artifact — a template/rubric the tuning model has to fill in, proving the change is
actually better rather than just fitting the sample it was scored against:
- Metric deltas across the scenario set (accuracy, safe-default rate; the UNSAFE-acts gate must
  stay 0 — a candidate that regresses it is disqualified outright, not just scored lower).
- Specific previously-failing scenarios that now pass, and specific previously-passing scenarios
  that now fail (regressions), named individually — not just aggregate numbers.
- A short natural-language justification from the tuning model for *why* it believes the change
  generalizes, not just why it scored well on this sample.

A human reads the rubric and decides whether to merge the prompt change — same review posture as a
riggers draft PR.

### Architecture-critique mode (separate from prompt tuning)

The system shouldn't stay laser-focused on prompt wording. Periodically (not every tuning session),
run a broader, less structured pass: hand a model the architecture docs, the current
scenario pass/fail breakdown, and recent failure patterns, and ask it to critique the *architecture*
— not propose a prompt tweak, but suggest structural ideas worth trying. Output lands as a dated
idea/report doc (same shape as `docs/ideas/*.md` or the `Dreams/*.md` reports) for human review —
read-only suggestions, never auto-actioned, since there's no deterministic "dispose" step for an
architecture idea the way there is for a scored prompt candidate.

## Budget & safety controls

- **Call budget is a per-tuning-session human decision**, set in config before a run starts — not a
  fixed global default. A cheaper model can reasonably get a larger budget for the same spend; the
  human sizes it per session based on which model(s) are in play.
- Every candidate evaluation in a generation runs concurrently against the budget ceiling — the
  OpenRouter backend is what makes this affordable/fast rather than serialized and rate-limited.
- v1 tuning never touches real MCPs or the real vault by default (see "Scoring during tool-loop
  execution").

## Open questions

- Exact shape of the shared `Scenario`/`ExpectKind` types once lifted out of `liberado-eval` —
  a small shared crate, or into `liberado-common`? Decide when the dispatcher-layer v1 is built.
- How many generations count as "not too deeply" per layer, per outer-loop cycle — likely starts
  as a tunable, defaulted low, and adjusted once real runs show how fast a layer converges.
- Whether/how the future history-informed scenario source (once there's real conversation-store
  data to learn from) should filter for anything sensitive before feeding it to an external model —
  revisit once there's enough real history for this to matter.

## Rough build order

1. ✅ **Done (2026-07-03).** Gave `liberado-eval` a `[lib]` target (`src/lib.rs`, auto-detected
   alongside `src/main.rs` — no `Cargo.toml` change needed, same shape `liberado-tui` already uses).
   `scenarios.rs` moved under the lib unchanged; the per-scenario classification logic that used to
   be inlined in `main.rs`'s run loop is now `liberado_eval::score()` (`src/scoring.rs`), returning
   a `ScenarioOutcome` — the actual "scoring shape" a future tuner needs, not just the scenario data
   types. `main.rs` calls it instead of re-deriving the classification; behavior is unchanged (same
   printed output), now backed by 5 unit tests on `score()` itself.
2. ✅ **Done (2026-07-03).** `liberado-provider-openrouter` — implements `Provider`, mirroring
   `provider-deepseek`'s shape almost exactly (same OpenAI-compatible wire format). `from_env()`
   reads `OPENROUTER_API_KEY`/`OPENROUTER_MODEL`. Not yet wired into any binary — scaffolded only.
   **Deliberately not de-duplicated against `provider-deepseek`** (near-identical translation
   logic in both crates) — see the new crate's `lib.rs` module doc comment for why and when to
   revisit. Live smoke test is `#[ignore]`d pending a session where the key is actually visible to
   the shell running the tests (env vars set after a long-lived shell starts don't propagate to it
   on Windows without a restart).
3. ✅ **Done (2026-07-03).** `liberado-heuristics-tuner` v1 — dispatcher-only, cold-start +
   mutation candidate generation, a beam-search-with-restarts loop over `liberado-eval`'s existing
   19 scenarios, budget-capped (`Budget`, `Arc<AtomicUsize>` + `Ordering::Relaxed`). Required first
   fixing a real blocker: `Dispatcher`'s system prompt was a private hardcoded `const`, with no way
   to test a candidate against the real `Dispatcher` type — `crates/dispatcher/src/lib.rs` gained a
   `pub const DEFAULT_SYSTEM_PROMPT` and a `with_system_prompt()` builder (backward-compatible, no
   existing test changed). The rubric (`rubric.rs`) is the "propose a diff, never auto-merge"
   artifact — metric deltas, named scenario regressions/fixes, a model-authored generalization
   justification, both prompts side-by-side — printed to stdout and saved under
   `liberado_config::data_dir()/tuner/`.

   **Config, revised same day**: every tunable except `OPENROUTER_API_KEY` (a secret — never in a
   file, Decision 10) now resolves through three layers, lowest to highest —
   code default → `tuner.toml` (`config.example/tuner.toml` is the template, resolved via the same
   `liberado_config::config_dir()` the daemon's own topology/policy/tuning files use) →
   environment variable — matching Decision 14's layering exactly, so per-session tweaking (model
   choice, beam width, budget, generation count) never needs a recompile. Default scoring/meta
   model changed from `deepseek/deepseek-chat` to `deepseek/deepseek-v4-flash` (smaller, cheaper,
   still DeepSeek so a winning prompt is likely to transfer).

   30 unit tests (pure logic: beam selection, budget accounting, aggregation, request-building,
   rubric formatting, file/env config layering) plus 2 `#[ignore]`d live tests
   (`generation::live_cold_start`, `search::live_end_to_end`) for whenever `OPENROUTER_API_KEY` is
   actually visible to the shell running them. Live-run the tuner itself:
   `cargo run -p liberado-heuristics-tuner`.

   **Per-generation output + a one-command runner, added same day** — driven by a real
   constraint: the user is running this remotely over SSH (Termux on a phone) and can't babysit a
   live session or easily hand a key back and forth. `run_tuner`'s `TunerResult` now carries a
   `generations: Vec<GenerationRecord>` (each generation's own best candidate + fitness + a rubric
   against the baseline, with its own justification call — not just the final winner), so a human
   reviewing later sees the search's progression, not just where it ended up. `main.rs` saves one
   file per generation (`generation-N.txt`) plus `final.txt` (same content as the last generation's
   file, so there's an obvious filename to check first) under
   `<LIBERADO_DATA_DIR>/tuner/<run-timestamp>/` — one folder per invocation. `scripts/run-tuner.ps1`
   wraps the whole thing into one command: it reads `OPENROUTER_API_KEY` from an already-exported
   env var (never accepted as a script argument, so it never lands in shell history or a process
   listing), builds/runs the tuner, and points at the output folder when done.
4. Proposal-rubric output format + human review flow — folded into step 3 above (the rubric
   *is* the output format; there's no separate "diff" artifact beyond the full candidate/baseline
   prompt text already in the rubric, which is what a human diffs by eye before hand-copying a
   change into `DEFAULT_SYSTEM_PROMPT`).
5. Extend the outer loop to the executor layer (mocked `ToolRuntime`), then main-agent.
6. Architecture-critique mode, as a separate, lower-frequency entry point into the same crate.
7. **Topology-driven scenario generation** (see "Scenario generation" above) — read the live
   `CapabilityCatalog` and synthesize goals per tool, weighted toward newly-added/reshaped ones,
   folded in alongside the fixed `liberado-eval` scenarios. This is the mechanism that actually
   delivers the "re-adapts when you add a tool" goal, not just "gets slightly better at 19 fixed
   goals" — identified as the real next step after reviewing the first live run's results.
8. **Multi-sample scoring** (see "Real-model verification — findings (2026-07-04)" below) — score
   each scenario across N samples per candidate instead of one, to average out real-model run-to-run
   noise observed during verification. A config knob (sample count), not a redesign. Worth doing
   before or alongside item 7 — noisy scoring undermines confidence in any tuning run regardless of
   scenario source.

## First real run — findings (2026-07-03)

A live session (defaults: `deepseek/deepseek-v4-flash`, 3 generations, beam width 2) took routing
accuracy from 0.72 to 0.94 and held it there, with `unsafe_acts` staying at 0 in every generation —
the hard safety gate never broke even while the search was actively wrong about other things.
Recorded here because the specific failure mode is a real, generalizable lesson, not a one-off:

- **What generalized cleanly**: generation 1 added two rules — "code-dispatch actions are
  reversible (draft PR only), route confidently" and "open-ended multi-document analysis needs a
  subagent" — fixing 5 of 6 originally-failing scenarios in one shot with no observed downside.
  Hand-adopted into `DEFAULT_SYSTEM_PROMPT` the same day (see dispatcher commit).
- **What didn't**: generation 1 fixed those five scenarios by *replacing* the baseline's general
  "bias toward safety" framing with a specific rule list — and the rule list didn't cover external
  actions, so it silently lost the safety net for anything not explicitly named, regressing
  `external-email` (expects `Clarify`). Lesson: a rule-list-style mutation is only as safe as its
  coverage; don't let a specific-case fix quietly delete a general safety principle. This is why
  the hand-adopted version *adds* the two rules to the existing prompt rather than replacing it.
- **A mutation that should never be adopted, and reveals a real scoring blind spot**: generation 3,
  trying to fix a second regression (`external-broadcast`, expects `Propose`), taught the model to
  emit `Propose` as a directly-choosable classifier action — but invented a JSON shape
  (`{"proposal":..., "reason":...}`) that doesn't match the real `ProposedAction` enum at all. In
  this codebase `Propose` is never something the classifier emits directly; it's produced only by
  the deterministic consequence guard downgrading a concrete `ExecuteDirect`. The tuner's scoring
  compares top-level action *labels* only, not full structural JSON validity against the real Rust
  types, so this defect wasn't caught by the score (a real gap worth closing eventually — but for
  now, a human reviewing before adopting anything is the actual defense, exactly as designed). The
  likely correct fix for `external-broadcast` isn't a classifier rule at all: classify concrete,
  nameable external actions as `ExecuteDirect` and trust the existing consequence guard to downgrade
  them to `Propose` automatically, rather than teaching the classifier to self-censor via `Clarify`
  or to fabricate an action type it was never meant to produce.

## Real-model verification — findings (2026-07-04)

Ran `liberado-eval` against the real `deepseek-chat` model to confirm the hand-adopted generation-1
rules (above) actually held. Two things came out of this, one good, one that changes how much
confidence to put in any single eval/tuning-run number going forward.

- **The architectural hypothesis about `external-broadcast` was right.** With the original
  baseline's "bias to safety" framing kept intact (not replaced, per the lesson above) plus the two
  hand-adopted rules, `external-broadcast` now resolves correctly via `Propose` on its own — without
  ever teaching the classifier that `Propose` exists. It classifies the goal as a concrete
  `ExecuteDirect` and the deterministic consequence guard downgrades it automatically. This is the
  fix generation 3 was reaching for (badly); the real one required no prompt change at all once the
  general safety framing wasn't sacrificed.
- **The first real verification run reproduced a genuine regression, and it was worth fixing**:
  the hand-adopted code-dispatch rule ("route confidently") didn't condition on the tool actually
  being in the catalog shown to the model, so `code_dispatch_not_configured` (grant exists, but the
  MCP isn't registered) flipped from `Clarify` to `ExecuteDirect` — a real `unsafe_acts: 1`. Fixed by
  qualifying the rule ("...only when a code-dispatch MCP actually appears in the catalog you were
  given"). Confirmed fixed across 5 consecutive live runs (`unsafe_acts: 0` every time).
- **Real-model runs are noisier than a single sample suggests, even at temperature 0.** Across those
  5 consecutive runs with an *unchanged* prompt, overall routing accuracy swung between 11/18 and
  16/18, and which scenarios failed changed run to run — including basic, unambiguous scenarios
  (`simple-task-add`, a single granted tool and one obvious step) that have nothing to do with the
  code-dispatch rule at all. DeepSeek's real API isn't perfectly deterministic run-to-run even at
  `temperature: 0.0` (likely server-side batching/routing, not a client-side issue). This matters
  beyond just this one fix: **any single eval or tuning-run accuracy number — including the
  0.72 → 0.94 jump in the first tuning session above — carries real sampling noise**, and a
  single-sample comparison between two candidates isn't fully trustworthy on its own. The
  `unsafe_acts` gate held solid across all 5 runs, though — that signal is far more stable than the
  raw accuracy number, likely because it's a rarer, more discrete event (most of the noise seems to
  land on borderline-confidence scenarios flipping between adjacent action labels, not on clearly
  right-or-wrong safety calls).
- **Candidate future improvement, not yet built**: score each scenario across N samples (e.g. 3-5)
  per candidate and use a majority vote or averaged fitness, rather than trusting one sample per
  scenario. Costs proportionally more calls per candidate, but given the budget is already
  session-configured (Decision: human sets it per run), this is a config knob, not a redesign. Worth
  doing before or alongside topology-driven scenario generation (item 7 above) — noisy scoring
  undermines confidence in any tuning run, current or future, regardless of which layer or scenario
  source is being tuned.
