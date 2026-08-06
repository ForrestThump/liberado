# Self-PR quality — roadmap toward light-oversight merges

**Status:** Living product roadmap (2026-08-06). Not a build-spec for a single PR; the *why* and
the ladder. Implementation slices should still be one-PR-sized and carry mutation evidence per
[`backlog.md`](backlog.md).

**Related:**

- Dogfood write-up: [`self-host-coding-dogfood-2026-08.md`](self-host-coding-dogfood-2026-08.md)
- Coding surface plan: [`coding-tui-plan.md`](coding-tui-plan.md) (S1–S7)
- Harness gaps: [`research/agentic-coding-harness-gap-analysis-2026-08.md`](research/agentic-coding-harness-gap-analysis-2026-08.md)
- Cold-review skill (operator): [`../../Skills/cold-review-pr.md`](../../Skills/cold-review-pr.md)
- Dream / memory skill: [`../../Skills/dream.md`](../../Skills/dream.md)
- Scoreboard: [`../roadmap.md`](../roadmap.md) Priority 3

---

## The bar

The system is "good enough" when we can ask it to open PRs on liberado itself and the default human
job is:

1. Read the PR body (intent + evidence).
2. Skim the diff for blast radius and taste.
3. Make **minimal** edits if any.
4. Merge.

Not: re-derive intent from the event stream, re-run the entire test matrix by hand, or re-discover
that the agent committed then reported "no changes."

That is a **product loop** problem more than a model-intelligence problem. Autonomy without a
definition of done produces volume, not merge-cheap PRs.

---

## Merge bar (what high quality means here)

A self-submitted PR is merge-cheap only if all of the following hold:

| # | Requirement | Today (develop, post-#72/#73) |
|---|-------------|------------------------------|
| 1 | Intent is clear and **frozen** before files move (contract / success criteria) | Intake exists; often disabled for dogfood; verifiers often empty |
| 2 | Work is **isolated** (branch + durable workspace; parent clean) | Durable `coding-worktrees/{session}` + HostLocal (S4); project auth (S3) |
| 3 | **Machine evidence** of done (tests / clippy / path checks) — not only a summary | Verifiers + completion gate exist; **not default-on** for self-host |
| 4 | A **cold** pair of eyes saw the diff without the author's story | Skill only (`Skills/cold-review-pr.md`); not a pack stage |
| 5 | Findings are **fixed or dismissed with reason** on the same branch | Manual outer-agent ritual; checkpoints make a fix pass safe |
| 6 | Human review is **residual** (intent, taste, blast radius) | Still often archaeological |

Layers 1–3 are partially built. Layers 4–6 are process and productization.

---

## Assets already in hand

Do not rebuild these; compose them.

| Asset | Role in self-PRs |
|-------|------------------|
| Coding goals + intake / contract | Stops pure vibes-coding before write |
| Project authorization | Safe self-host on declared roots only |
| Durable session worktrees + HostLocal | Park/resume and checkpoints share one FS root |
| Shadow-git checkpoints + mid-build resume + rewind | Fix passes and human steering mid-build |
| Fan-out + parent LLM merge-back (hub-spawned, concurrency 3) | Width for multi-concern work |
| Verifiers, progress guards, optional completion gate | Partial definition of done |
| Explore mode (read-only PathPolicy / catalog) | Natural substrate for a cold-review child |
| Self-host dogfood on life-os | Only honest training ground (Windows paths, `gh` base, etc.) |
| `Skills/cold-review-pr.md` | Correct *shape*: cold report → filter → fix |
| Dream skill | Shape for durable memory — not yet wired to PR outcomes |

"Close" means the **loop almost exists**. It does not mean unattended merge is safe.

---

## Why human time still burns

Failures that force more than minimal edits are rarely "model dumb." Typical patterns from dogfood
and harness work:

1. **Definition of done is soft** — session `Succeeded` while CI or scope would fail review.
2. **No mandatory cold review in-product** — author-context agents miss their own assumptions.
3. **No tight fix-then-reverify loop** — review → filter → fix → re-test → only then ready PR is
   still an outer-agent ritual.
4. **Repo intel / headless packaging still thin** — rediscovering layout and CI expectations costs
   turns and invites drive-by diffs.
5. **Gate / critic defaults** — expensive quality machinery stays opt-in; soft success still ships.
6. **Blast radius policy** — missing PR size budgets and "don't touch unrelated crates" defaults.
7. **Self-improvement is manual** — Dream exists; failed PR classes do not automatically promote
   into verifiers or path rules.

---

## Ladder (do not skip)

Each layer multiplies the previous. Starting at "self-improvement" or "auto-merge" without A–C
teaches the system the wrong habits.

### Layer A — Mergeable unit of work (highest ROI)

Every self-host coding goal produces a **PR package**, not only a transcript.

**Ship package (default for project `liberado`, optional elsewhere):**

- Frozen contract with an explicit **verifier list** (minimum viable for self-host: targeted
  `cargo test -p …` / clippy on touched crates, plus "no drive-by files" where practical).
- Branch from integration base (`develop` for this repo's dogfood), durable worktree, commits that
  map to contract items.
- **Preflight** before `gh pr create`: verifiers green, base branch exists on origin (already a
  known footgun), diffstat under a size budget.
- PR body template: intent, mutation evidence, test-plan checkboxes (see PR #68 style).

**Exit criterion:** A human can merge most dogfood PRs after reading the PR body and skimming the
diff — without re-deriving intent from the SSE stream.

### Layer B — Cold review as a product stage

Productize `Skills/cold-review-pr.md` as a hub phase, not only markdown for outer agents:

```
build → verify → (draft PR optional)
     → cold-review child (no author context; explore / read-only tools)
     → filter findings (second model or stricter rubric; cite code to keep)
     → fix high/medium on same branch (checkpoints already support re-entry)
     → re-verify
     → mark ready for human
```

**Hard rules:**

- Cold reviewer must **not** see the goal narrative or prior tool trace — only diff + file reads.
- Filter must **cite code** when retaining a finding (kills hallucinated issues).
- Default **one** fix round; escalate to human if still red (avoids thrash).

**Exit criterion:** High-severity findings that survive the filter are almost always real; false
positive rate is measured on dogfood, not assumed.

### Layer C — Human as steering, not labor

Surfaces that match how merge actually happens:

- Queue of draft / ready PRs with residual risk summary (`awaiting_human` or equivalent).
- One action path: approve / request changes / kill (API first is fine; TUI/WebUI later).
- Optional later: auto-merge when label + CI green + cold-review clean + size budget.

**Exit criterion:** Oversight is residual judgment, not archaeology.

### Layer D — Self-improvement (only after A–C are boring)

- After merge or reject: short postmortem (what fooled review; which verifier was missing).
- Scheduled Dream-style consolidation of those notes into durable docs.
- Promote **repeated** failures into default verifiers or path rules — not more prompt prose.

Do not start here. Improving folklore is worse than no memory.

---

## How Liberado should own the loop

Liberado is a Life OS with a coding pack, not "another coding agent." The loop should be hosted:

| Piece | Owner |
|-------|--------|
| Contract + verifiers + worktree + PR package | Coding domain pack |
| Cold-review child | Hub-spawned explore (or a thin `review` domain) with read-only tools |
| Fix pass | Same coding session / branch (checkpoints + durable worktree) |
| Queue / approve | Goals API + face (`/goal`, park/resume, message); surfaces later |
| Policy | Topology projects + path budgets + "PR only under X" |

Outer tools (Grok `/review`, manual `Skills/cold-review-pr.md`) are prototypes. Self-host dogfood
must not depend on an outer CLI skill forever.

---

## What not to do

- **More model / more autonomy first** — raises PR volume faster than merge quality.
- **Auto-merge without cold review + CI** — one bad self-PR trains the wrong habit.
- **One mega-agent that both reviews and fixes in the same context** — loses the cold property;
  author contamination returns.
- **Skipping dogfood on liberado itself** — toy repos hide Windows extended paths, worktree Drop,
  `gh --base`, and real CI cost.
- **Empty verifiers with a green gate later** — measure with hard checks on, not soft summaries.

---

## Honest proximity

| Bar | Status |
|-----|--------|
| Agent can implement a scoped task and open a PR | **Yes** (with steering) |
| PR is usually *plausible* | **Yes** after dogfood reliability + fan-out + checkpoints |
| PR is usually *merge-minimal* | **Not yet** — missing default ship package + productized cold review + fix pass |
| Self-improving with light oversight | **Architecture exists** (skills, gates, dream); **loop not closed** |

The feeling of "almost there" is correct for the *substrate*. The remaining work is closing the
loop so that substrate becomes a repeatable, review-cheap path.

---

## Ranked investments (toward the dream)

Order is deliberate. Prefer shipping the first items as small PRs with dogfood evidence.

| Rank | Investment | Layer | Notes |
|------|------------|-------|--------|
| **1** | **Ship package** on coding goals (PR body, preflight tests, size/scope gate; default-on for `liberado`) | A | Highest ROI; unblocks honest dogfood grading |
| **2** | **Productize cold-review-pr** as hub stage (explore child + filter + one fix round) | B | Uses explore mode + checkpoints already on `develop` |
| **3** | **Default verifiers** for self-host (touched-crate test/clippy; non-empty when project is set) | A | Soft `Succeeded` is the main merge tax |
| **4** | **Repo map / cheap intel** | A–B | Fewer turns rediscovering layout; fewer drive-bys |
| **5** | **Human queue UX** (API list + status first) | C | Steering without reading full streams |
| **6** | **Post-merge Dream on dogfood outcomes** | D | Only after 1–3 reduce noise |
| **7** | Completion gate default-on for self-host *after* cost/quality measured | A–B | Still opt-in until S7-style measurement (see coding-tui plan) |

A natural first implementation PR is **(1)+(2) as one "PR quality loop"** on coding goals: verify →
cold review → fix → re-verify → draft/ready PR. Substrate: explore mode, durable worktrees,
checkpoints/mid-build resume (#73), hub fan-out patterns (#72).

---

## Suggested acceptance for "we can just review and merge"

Treat this as the dogfood grade for the program, not a single ticket:

1. **Five consecutive self-host PRs** on liberado (scoped tasks) reach "ready" with:
   - green preflight verifiers,
   - cold-review stage run,
   - zero high findings open (or explicitly waived with reason in PR body).
2. Human time per PR is dominated by **taste and scope**, not by rediscovering what the agent did.
3. At least one PR in the set required a **mid-build park/resume** or **fix pass after cold review**
   and still landed clean — proving the recovery path, not only the happy path.
4. Rejected or heavily amended PRs leave a **one-paragraph postmortem** that becomes a verifier or
   path rule within one week (Layer D seed).

When (1)–(3) hold for a month of casual use, Layer C auto-merge becomes a reasonable experiment.
Until then, draft + human submit remains the default.

---

## Relationship to existing slice labels

| Existing label | How this roadmap maps |
|----------------|------------------------|
| S1 completion gate | Quality floor inside an attempt; still opt-in; not a substitute for ship package |
| S2 goal surface | Needed for human steering of the queue (C) |
| S3 project auth | Landed; required for safe self-host |
| S4 checkpoints / mid-build resume | Landed (#73); enables B fix pass and park/resume dogfood |
| S6 fan-out | Landed (#72); width, not quality loop |
| S7 intake / verifiers as authoritative | Contract + verifiers are the spine of Layer A |
| E6-c(b) | Satisfied in spirit by S4 mid-build resume with checkpoints |
| C2 dogfood | Continues as the grading method for this entire roadmap |

This document does not replace [`coding-tui-plan.md`](coding-tui-plan.md); it ranks **PR quality and
self-improvement** as the product outcome those slices were building toward.

---

## Changelog

| Date | Note |
|------|------|
| 2026-08-06 | Initial roadmap after #72 (fan-out), #73 (checkpoints/mid-build resume), and discussion of cold-review skill vs in-product loop |
