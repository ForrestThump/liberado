---
kind: finding
status: historical
authority: evidence
domain: delegation
canonical_for: delegation-failure-modes
open_items: false
---

# How delegated PRs go wrong

Observed over 15 PRs across three rounds (2026-08-01 → 08-03), written by **Grok** (#33–#37, #39)
and **DeepSeek** (#40–#45). Attributed by model because the patterns differ enough to change how you
review — but treat the attribution as a hypothesis, not a law. Sample sizes are single digits, and
several failures are probably general to delegated work rather than to a model.

**The one thing common to every PR in all three rounds:** the tests were aimed at the mechanism the
implementer had in mind, not at the guarantee a user experiences. Nothing else generalised as well.

---

## Grok — over-claims

Writes more ambitious code, usually correct, and then describes it as doing more than it does. Review
budget goes on **checking claims**.

| # | What happened |
|---|---|
| #34 | `token_usage_total` filled with a lifetime sum, while both consumers render it *against* `context_window` as a percentage. A one-event fixture could not distinguish the two readings. |
| #35 | An entire acceptance item (the unanswered-turn note) shipped with **no test**. Deleting the feature left all five tests green. |
| #36 | A test named for the zombie `turn_running` case whose fixture was *also* a lost turn, so it failed through the other clause. The guard it was named for could be deleted undetected. |
| #37 | `work_start_inventory_is_documented_in_module_docs` **could not fail** — `include_str!` on its own module, with the needle array inside the file being searched. |
| #37 | A durability gap masked by polling for 2s after the drain returned — time production does not have. |
| #39 | A test named "findings reach the face" whose assertions passed with the feature removed: a scripted mock ignores its prompt, so no in-process test can prove that claim. |
| — | Of three branches stating an R1 mutation in a doc comment, **two were wrong when actually run**. |

## DeepSeek — omits

Writes narrower code and describes it accurately. Review budget goes on **finding gaps**.

| # | What happened |
|---|---|
| #40 | Changed three call sites, tested two — and the untested one was the busiest (plain `DispatchSubagent`). Reverting it left all 108 suites green. |
| #41 | `repeat_calls` stamped at `submit_report` decode rather than after the batch. Four otherwise well-aimed tests all put `submit_report` in its own response, so the fixtures agreed with each other about batch shape. |
| #42 | Asserted a log **level** with a `#[cfg(test)]` flag stored beside the macro. Demote `info!` → `debug!`, leave the store, test stays green — and the level *was* the deliverable. |
| #43 | Fixed the catalog/goal ordering but left the (also stable) zone block after the goal. Correct as far as it went; the same mistake one block down. |
| #44 | Journaled a **running** counter into a field the rollup **sums**. Its test hand-built `LatencyEvent`s encoding the summing assumption and never ran the executor. Real run: truth 2, journal `[None, None, Some(1), Some(2)]`, rollup 3. |

**Worth crediting:** #40 and #41 both declared an unobtainable acceptance item plainly rather than
inventing a number. That is the behaviour to reinforce — an honest "not done, here's why" is a pass.

---

## The mitigation that matters

**Per-site mutation evidence does not catch the dominant DeepSeek failure**, and it is worth being
precise about why. Mutating your own code and re-running your own fixture proves the fixture is
*sensitive to that line*. It says nothing about whether the fixture models reality. Three of the four
recent defects survived exactly that check:

- #44's fixture summed hand-built events — mutate the executor all you like, the fixture never ran it.
- #42's flag sat next to the macro — mutate the level, the flag still gets stored.
- #43's prompt had no zone block — mutate the ordering, there is nothing to be out of order.

**The rule that does catch them: if you can drive the real code path, never hand-build the
intermediate.** Every one of those three becomes impossible when the test runs the executor, captures
the emitted event, or builds a request that actually contains the block being ordered.

Stated as a question to answer before writing the assertion — and answer it against a *different*
implementation, not the same one:

> **Would this fixture pass if the feature were implemented the wrong way?**

If you cannot answer, the test is decorative.

## Review heuristics that paid off

- **Read what the test constructs, not what it asserts.** Every defect above was visible in the
  fixture setup, not the assertion line.
- **When a test builds a struct the production code normally builds, be suspicious.** That is the
  tell for "fixture agrees with the implementation."
- **Check the paths changed against the paths tested, by count.** #40 was three vs two. It takes ten
  seconds and found the highest-impact bug in that batch.
- **When a fix follows a pattern, look for the next instance of the pattern.** #43 fixed one of two
  stable blocks; the seam bug's `delegate` path had improvements miss it twice for the same reason.

## Cost note

Delegation saved roughly 15–20% of output tokens on mechanical work with a decided design and
verified pointers. It saved nothing where the task needed judgement about what a value *means* to its
consumers (#34), and it lost outright once when a spec was written against a stale status line for
work already on main (round 3 §1). **Spec accuracy is a bigger lever than model choice.**
