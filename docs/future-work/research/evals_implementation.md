# Evals — a third-party note, and what we decided to do about it

**Source**: an Anthropic engineer's public write-up on building a self-improving eval loop, quoted in
full below. **Not** a Liberado document; kept because the argument is good and the decision it
prompted is worth recording.

**Decision (2026-08-03): do not build an eval harness yet.** Reasoning, and the cheaper thing built
instead, are below the quote.

---

## The source

> "The biggest mistake in AI right now - people are building graphs and loops without self-improving
> eval agent
>
> We made that mistake at Anthropic - It cost us 2 years"
>
> here's how he build it, step by step:
>
> step 1 → take 50 real user prompts - run your agent - if it passes 80%+, your eval is too easy -
> sweet spot is 50%
>
> step 2 → every failed run is a transcript - feed it to Haiku: "what went wrong?" - your eval set
> builds itself for free
>
> step 3 → score two things: right answer AND right path - right answer, wrong path = breaks next week
>
> step 4 → new model drops, run the same eval - his +9% was fake - 6% was the model dodging a bug -
> the transcript caught it, the dashboard didn't
>
> step 5 → plug evals into CI - no green evals = no deploy - this is the loop that fixes itself

---

## What applies here, and what does not

**Step 3 is the strongest, and this repo has already proved it.** *"Right answer, wrong path = breaks
next week"* is exactly [the delegate seam bug](../archive/delegated-work-is-discarded-at-the-seam.md): an
accurate, well-structured answer produced by a path that discarded the research and reconstructed the
specifics from the model's priors. **Any output-quality grader would have scored that a pass.** What
was false was the provenance, not the content.

**Step 2 is the weakest for us.** "Feed failed transcripts to a model, the eval set builds itself"
finds failures that announce themselves. The seam bug never failed — it succeeded and looked good. A
model asked "what went wrong?" would have said "nothing."

**Step 5 is wrong for us right now.** Evals in CI means real inference on every PR, while
[P1.5 token economics](../../roadmap.md) is unpicking a 56% line item. Note that the live conformance
suite is deliberately **hand-run on the box**, not CI-gated, for the same reason.

**Step 1's 50%-pass calibration** is the right principle, but presupposes variants to compare.
Measuring one configuration against itself tells you the tasks are hard, not what to change.

## The actual gate: a free oracle

What decides whether an eval pays for itself is whether something grades the run **without a human or
a second model**.

| | free oracle? |
|---|---|
| Agentic coding | **yes** — tests pass or fail; the compiler is objective |
| Life-OS tasks (calendar, vault, research) | **no** — grading means a human reading transcripts, or an LLM judge with its own failure modes |

That is why this work belongs with the coding pack rather than before it.

**But the trigger is earlier than "the coding pack is mature."** It is **TE1**: narrowing the tool
catalog to cut 56% of token spend is the first change that deliberately trades quality for cost, and
the first time *"did that make it dumber?"* has to be answered with a number — from a baseline taken
**before** the change.

## What was built instead

`liberado-cost provenance-ratio` ([`crates/cost/src/lib.rs`](../../../crates/cost/src/lib.rs)) —
per delegation, the ratio between what the face agent **received** and what it then **wrote**, read
from session logs the daemon already writes. No inference, no grader, no authored tasks, nothing to
maintain.

On the live logs it ranked the known seam conversation **first, at 29.4×**, without being told about
it — against a median of 0.9×. Six of seventy-eight delegations flagged: a short list worth reading,
which is the cheapest useful thing an eval can do before a free oracle exists.

Sibling: `liberado-cost delegation-cost`, the same idea for
token cost.

**The pattern worth generalising:** the system already writes down what it did. Reading that is
cheaper than commissioning new runs to generate it, and it found two real problems in one week. Reach
for a harness when a question genuinely cannot be answered from existing records.

## When to revisit

- **TE1 lands a catalog-narrowing change** → a before/after quality baseline is needed. First real customer.
- **The coding pack takes real work** → tests and the compiler are the free oracle. Build it then.
- **A model swap is proposed** → step 4's warning is the one that generalises best. Compare transcripts, not dashboards.
