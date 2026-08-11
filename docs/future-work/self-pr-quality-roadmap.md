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

- Frozen contract; attempt-level verifiers as needed during the build loop.
- Branch from integration base (`develop` for this repo's dogfood), durable worktree, commits that
  map to contract items.
- **Preflight** (see [Generic preflight gate](#generic-preflight-gate)) before ready / `gh pr create`:
  **CI-equivalent ship bar** on the agent host — not a soft summary. For liberado that means the
  same commands CI runs (full workspace test matrix, clippy, fmt, deny), plus hygiene (base branch
  exists, path/diffstat budget, no secrets). Ten–thirty minutes is acceptable if it saves a human
  hour and a thrashy fix cycle.
- PR body template: intent, mutation evidence, preflight report, test-plan checkboxes (see PR #68
  style).

**Exit criterion:** A human can merge most dogfood PRs after reading the PR body and skimming the
diff — without re-deriving intent from the SSE stream or re-running the suite by hand.

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

## Generic preflight gate

Preflight is **not a Rust feature** and should not live only as hard-coded `cargo test` inside the
coding pack. It is a **product concept** any pack can call:

> Nothing is *done / ready / shippable* until preflight for that project (or task profile) passes.

Other agentic harnesses often skip a named preflight stage (pair-programmer UX, CI-after-PR as the
real gate). For light-oversight self-PRs that is the wrong trade. Liberado should make preflight
first-class and **language-agnostic at the gate**, with project-specific steps for the repo.

### Two layers

| Layer | Responsibility |
|-------|----------------|
| **Product / kernel** | `PreflightRunner`: resolve a `PreflightSpec`, run ordered steps, fail closed, emit events, return a `PreflightReport`. Packs call this before terminal success that implies ship. |
| **Project binding** | What “ship” means for *this* root: commands, timeouts, profiles (`ship` / `fast` / `deep`). Declared in topology / project config (or a script those steps invoke). |

Coding pack is a **client** of preflight, not the owner of “how to build Rust.” Life, dispatch, and
future domains reuse the same hook with different specs.

### Do not implement preflight as “run the GitHub Actions YAML”

Running `.github/workflows/ci.yml` via `act` (or by re-implementing Actions in-process) is
attractive and incomplete:

| Approach | Verdict |
|----------|---------|
| Local Actions runner (`act`, etc.) | Partial fidelity: matrices, `ubuntu-latest`, secrets, services, sibling checkouts drift from real CI |
| Push + `gh run watch` on the real workflow | True CI; slow; needs remote; chicken-and-egg with “preflight before PR” unless draft-first |
| **CI and agent both call the same entrypoint** | **Preferred:** shared script or config-defined steps; YAML stays a thin orchestrator |

**Semantically** preflight should mean “what CI requires to merge.”  
**Mechanically** prefer:

```text
scripts/preflight.sh   (or preflight.ps1 + sh, or one make/just target)
        ↑                         ↑
  GitHub Actions step       liberado PreflightRunner
```

The agent does not re-encode clippy flags in Rust. Drift becomes impossible once CI only invokes
the shared entrypoint.

Optional later: `mode = "github_actions_remote"` (push branch, watch workflow) for multi-OS truth
when local host coverage is not enough. That is a **profile**, not the default engine.

### Sketch: spec and report

Illustrative only — not a frozen API:

```text
PreflightSpec {
  id,                    // "ship" | "fast" | "deep"
  steps: [               // ordered, fail-fast
    { name, run, cwd?, env?, timeout, required }
  ]
}

PreflightReport {
  ok,
  steps: [{ name, exit_code, duration, log_excerpt_or_path }],
  summary
}
```

Project config sketch (`topology` / projects — shape TBD at implement time):

```toml
[[projects]]
name = "liberado"
root = "…"

[projects.preflight.ship]
# Prefer one script CI also calls; expanded steps are fine for small repos.
steps = [
  { name = "fmt",    run = "cargo fmt --check" },
  { name = "clippy", run = "cargo clippy --workspace --exclude liberado-webui --all-targets -- -D warnings" },
  { name = "test",   run = "cargo test --workspace" },
  { name = "deny",   run = "cargo deny check" },
]
```

Another project might use `npm test && npm run lint` — **no Rust in the abstract gate.**

### Where it sits vs verifiers and the completion gate

| Mechanism | Job |
|-----------|-----|
| **Attempt verifiers** | Small, fast, in-loop (paths exist, one command) |
| **Completion gate** | Model quorum on “is the claim good?” (expensive, optional) |
| **Preflight** | **Repo/project ship bar** — CI-equivalent (or declared steps) before ready/PR |
| **Remote CI** | Multi-OS, secrets, final merge protection |

Flow:

```text
build loop (cheap verifiers)
  → optional completion gate
  → PREFLIGHT (ship bar)
  → open draft / mark ready / allow gh pr create
  → remote CI still runs
```

### When packs run it

Generic hook (name TBD):

```text
before_terminal_success / before_ship:
  if project.preflight required for this outcome:
    report = preflight.run(profile)
    if !report.ok → Failed or stuck+ask — never Succeeded + open ready PR
```

| Task | Preflight meaning |
|------|-------------------|
| Coding → PR | Project `preflight.ship` (for liberado: full CI-equivalent matrix) |
| Proposal / vault apply | Dry-run, schema, path policy |
| Cron outbound | Template render + empty-check |
| Dispatch child “done” | Child exit + required artifacts |

### Liberado `preflight.ship` content (default bar)

Align with [`.github/workflows/ci.yml`](../../.github/workflows/ci.yml) until CI is thinned to a
shared script:

| Step | Hard gate | Notes |
|------|-----------|--------|
| `cargo fmt --check` | Yes | Same as CI |
| `cargo clippy --workspace --exclude liberado-webui --all-targets -- -D warnings` | Yes | Same flags as CI |
| `cargo test --workspace` | Yes | Full matrix; includes layer-rules; time is OK |
| `cargo deny check` | Yes | Cheap; supply-chain |
| Base branch exists / hygiene / diffstat budget | Yes | Not in CI yaml; agent ship extras |
| Full multi-OS matrix on one host | No | Document “preflight = this host”; remote CI owns the other OS |
| Line coverage threshold | Later | Report first; hard-gate only after stable (prefer diff coverage) |
| Mutation testing | Later / deep profile | Full workspace mutants are hours; optional touched-crate `deep` profile or scheduled campaign — not every ship preflight v1 |

**Agent must not edit preflight or delete tests to pass** — path policy / refuse mutating
preflight scripts and `.github` without human, or pin expected step hashes from topology.

### Why full matrix (not “touched crates only”) for liberado ship

Interactive harnesses often run targeted tests for latency. For self-host liberado with a
merge-cheap bar:

- Soft `Succeeded` without full green is the main human tax.
- Workspace tests include **layer-rules** (architecture), not only unit tests.
- One red CI + human context switch usually costs more than 10–30 minutes of local
  `cargo test --workspace`.
- “Targeted only” remains a **`fast` profile** for docs-only or explicit opt-in — never default
  for code PRs on project `liberado`.

### Implementation order (preflight-specific)

1. **`PreflightRunner` + config-driven steps** — pack-callable; coding blocks ready/PR until ok.
2. **Wire liberado `ship` steps to match CI** — full matrix + clippy + fmt + deny.
3. **Thin CI yaml to call the same script/steps** — single source of truth.
4. **Profiles** — `fast` / `deep`; optional remote workflow watch.
5. **Reuse from other domains** — same runner, different specs.

---

## How Liberado should own the loop

Liberado is a Life OS with a coding pack, not "another coding agent." The loop should be hosted:

| Piece | Owner |
|-------|--------|
| Contract + attempt verifiers + worktree + PR package | Coding domain pack |
| **Preflight (ship bar)** | Shared runner + project config; coding (and others) call it |
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
- **Hard-code cargo into the kernel** — preflight is generic; Rust is a project binding.
- **Treat `act`/YAML re-execution as the only engine** — share entrypoints with CI instead.
- **Let the agent weaken the suite to green** — failing preflight is a stop, not a prompt to
  delete tests or skip clippy.

---

## Honest proximity

| Bar | Status |
|-----|--------|
| Agent can implement a scoped task and open a PR | **Yes** (with steering) |
| PR is usually *plausible* | **Yes** after dogfood reliability + fan-out + checkpoints |
| PR is usually *merge-minimal* | **Not yet** — missing default ship package + productized cold review + fix pass |
| Self-improving with light oversight | **Architecture exists** (skills, gates, dream); **loop not closed** |

> ### ✅ The ship gate reached the dispatch path (found 2026-08-11, fixed in PR #134)
>
> Investment **#1** below — `PreflightRunner` plus the ship profile — **landed in PR #74**, reached
> through `session_pack::build` → `preflight_hook::run_ship_preflight`.
>
> **`crates/acp-bridge/src/coding_run.rs` did not use `session_pack`.** It mentioned
> `CodingSessionPack` twice, both times in comments describing what it mirrored. So every run
> dispatched over ACP — every dogfood run since the Paseo integration landed, including all of the
> 2026-08-10/11 A/B work — **skipped the ship bar entirely.** That is the config-shadow failure class
> at the level of a subsystem rather than a setting: built, tested, green, unreachable from the path
> in use, and the most likely single explanation for why every dispatched run needed hand-finishing.
>
> **Two lessons survive the fix, and the second is the transferable one.**
>
> First: `ship_preflight_required_for` / `ship_spec_for` now take a bare payload and the `GoalSpec`
> versions delegate to them, so the two entry points cannot be held to different bars.
> `ProjectConfig::ship_preflight_payload()` is the one builder both use.
>
> Second, and larger: **wiring the gate was not enough, because the bridge was loading no
> configuration at all.** It read `LIBERADO_CONFIG_DIR` directly instead of calling
> `liberado_config::config_dir()`, so it opted out of the other three resolution tiers, and nothing
> in any launch path set the variable. No topology meant no declared project, which means the gate
> would have been wired and *still* inert. Ask "was the config loaded" before asking "is the feature
> reachable" — the bridge now logs its resolved config dir and which files it found, precisely
> because this failure was silent.

The feeling of "almost there" is correct for the *substrate*. The remaining work is closing the
loop so that substrate becomes a repeatable, review-cheap path.

---

## Ranked investments (toward the dream)

Order is deliberate. Prefer shipping the first items as small PRs with dogfood evidence.

| Rank | Investment | Layer | Notes |
|------|------------|-------|--------|
| **1** | **Generic `PreflightRunner` + liberado `ship` profile** (CI-equivalent full matrix, clippy, fmt, deny; blocks ready/PR; PR body + size/scope) | A | Highest ROI; abstract gate + project binding; see [Generic preflight gate](#generic-preflight-gate) |
| **2** | **Thin CI to the same entrypoint** as preflight | A | Single source of truth; no Rust in the runner |
| **3** | **Productize cold-review-pr** as hub stage (explore child + filter + one fix round) | B | After ship bar is honest; uses explore + checkpoints |
| **4** | **Repo map / cheap intel** | A–B | Fewer turns rediscovering layout; fewer drive-bys |
| **5** | **Human queue UX** (API list + status first) | C | Steering without reading full streams |
| **6** | **Post-merge Dream on dogfood outcomes** | D | Only after 1–3 reduce noise |
| **7** | Completion gate default-on for self-host *after* cost/quality measured | A–B | Still opt-in until S7-style measurement (see coding-tui plan) |
| **8** | Coverage / mutants as `deep` or scheduled — not default ship v1 | A | Report first; touched-crate mutants later |

Natural first implementation: **(1)** alone (runner + coding pack hooks + liberado steps matching
CI + PR body). Then **(2)** so yaml cannot drift. Then **(3)** cold review on top of a real ship bar.
Substrate already on `develop`: explore mode, durable worktrees, checkpoints (#73), hub fan-out (#72).

---

## Suggested acceptance for "we can just review and merge"

Treat this as the dogfood grade for the program, not a single ticket:

1. **Five consecutive self-host PRs** on liberado (scoped tasks) reach "ready" with:
   - green **preflight.ship** (full CI-equivalent matrix on agent host + hygiene),
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
| S7 intake / verifiers as authoritative | Contract + attempt verifiers during build; preflight is the ship bar above them |
| E6-c(b) | Satisfied in spirit by S4 mid-build resume with checkpoints |
| C2 dogfood | Continues as the grading method for this entire roadmap |
| [`verifiers.md`](../spec/architecture/verifiers.md) | Attempt-level gates; preflight is the project-level ship gate (complementary) |

This document does not replace [`coding-tui-plan.md`](coding-tui-plan.md); it ranks **PR quality and
self-improvement** as the product outcome those slices were building toward.

---

## Changelog

| Date | Note |
|------|------|
| 2026-08-06 | Initial roadmap after #72 (fan-out), #73 (checkpoints/mid-build resume), and discussion of cold-review skill vs in-product loop |
| 2026-08-06 | **Generic preflight gate:** CI-equivalent full matrix for liberado ship; abstract `PreflightRunner` + project binding; prefer shared scripts over running Actions YAML; coverage/mutants as later/deep; re-ranked investments |
