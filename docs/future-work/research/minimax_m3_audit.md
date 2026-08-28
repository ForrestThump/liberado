Got it — cloning it now.

Cloned it (200K+ LOC, 51 crates, 1,153 commits, last commit minutes ago). Here's the unvarnished read.

## What this repo gets right

**1. The failure-modes doc is a moat.** `docs/spec/architecture/failure-modes.md` documents 8 classes of bugs that *all shipped with green tests* — including the famous "the test was pointed at the wrong object" pattern, where `JsonlStore` was passing conformance while production used `SessionStore`. The companion rule — *"break the fix, watch the test fail, restore it"* — is enforced as a workflow norm, not just a comment. Most codebases don't even name their failure modes; you wrote a taxonomy with patterns, checks, and historical incidence (148 mutants, 66 caught, 39 missed in the first session run).

**2. Engineering gates that are actually mechanical, not aspirational.** Layer rules are enforced by `crates/test-support/tests/layer_rules.rs` — parses every `Cargo.toml`, reads the `role =` tag, asserts dependency direction. CRAP is a per-function ratchet (not just a ceiling), so a function at 50 going to 60 fails. `cargo metadata --locked` is a gate (catches the "green but lockfile not committed" footgun). Cross-platform footguns (Windows junctions + `git worktree remove` deleting the original sibling checkouts) are catalogued in `AGENTS.md` after they actually bit.

**3. Token economics before knobs.** Your roadmap explicitly says: *"Do not add tuning knobs before a measurement shows that a constant is wrong."* Then a dated findings doc, then a measuring step before the next. That's a rare discipline — most agent projects ship config flags because someone *imagined* a need.

**4. The provenance loop-break is a real design win, not a slogan.** `WriteProvenance` rides MCP request `_meta`, lands in the audit log, daemon attributes by content hash. The whole interaction has a `provenance_e2e` test. The diagram in `overview.md` shows the dashed return path being explicitly *suppressed* — that's the actual hard problem in vault-watching agents, and you solved it.

**5. Domain-neutral kernel with one second-domain proof.** `LifeOpsDemoRunner` sitting on `liberado-session` is the pigeonhole-detector move. Most "agent frameworks" never get a second domain running, so the abstraction is never tested by counterexample. You have it.

---

## Where it leaks

**1. Two god-crates contradict the modularity story.** `crates/coder-tools/src/lib.rs` is **5,887 lines**. `crates/executor/src/lib.rs` is **5,347 lines**. `crates/orchestrator` and `crates/coder-agent` are 3,266 and 3,065. The architecture overview brags about narrow waists and capability tiers, but the largest units are still monoliths. A system this strict about layering should also be strict about file size.

**2. ~3,800 panic points in non-test code.** 3,056 `.unwrap()` + 712 `.expect()` in production paths. For a project whose central thesis is *"safety is engineered, not prompted"* (Pillar 2 in your own overview), this is dissonant. Each unwrap is a place where the LLM-driven executor can take down the daemon. Not all are avoidable, but the ratio is a leading indicator of unmodeled failure modes — exactly the kind that won't show up in the conformance suite.

**3. Co-development is structurally fragile.** The workspace **does not build** without two sibling checkouts (`turbovault/`, `turbomcp/`) at unpinned local refs. The `[patch.crates-io]` block redirects four `turbomcp-*` crates to a local fork, with a comment saying *"Remove once turbomcp publishes the change."* That comment has probably been there for a while. Every new contributor hits this on day one. Every CI failure mode on a sibling's `develop` branch is now your problem.

**4. Bus factor of 1.** 734 commits from you, 239 from `harness-compare` (a bot), 173 from `ForrestThump` (also you, different account). The repo is a one-person system that *documents* the team-of-one honestly — "homelab status" etc — but the durability story for a project this size is thin. The 891 commits in 30 days (~30/day) is also a flag. Volume is fine if every commit is reviewed; that velocity makes it impossible for one person to actually review their own work with the rigor the failure-modes doc demands.

**5. Configuration gates that silently disable safety.** Your own class-2 failure mode is *"the guard that was off by default"*: zone declaration was optional, no MCP ever declared a zone, two layers of defense silently inert for months. The fix is good (now refuses to boot). But the existence of that class — and the fact that the config has *three* optional files plus TOML layers — tells you the failure mode will recur in new optionals. The "no zone ⇒ zero authority" fallback is the only safe default; you should audit every other opt-in to confirm the same pattern.

**6. The market position is a confession.** The roadmap says coding is *"good-enough-and-integrated, not a Claude Code / Kilo rival."* Realistic — but it also means Liberado is a personal Life OS competing for the time of one operator. The TUI/WebUI/CLI/Telegram surface area is large for that audience. If you ever want a second user, the Telegram-approvals UX and the per-`data_dir`/per-config TOML sprawl become friction that doesn't pay back at n=1.

**7. Documentation debt is large and openly so.** 173 markdown files, 31K lines. The CATALOG frontmatter discipline and the "Status:" header convention help, but `AGENTS.md` itself says: *"docs/ is large and not uniformly current."* The "Future-work archive/" directory exists precisely because the system is shedding dead planning. The signal is real, the cost of holding it in your head is also real.

---

## Bottom line

This is a deeply serious project with an unusually honest culture about its own failure modes — and a couple of structural choices that work against the design it claims. The kernel and the failure-modes discipline are the real asset. The god-crates, the unwrap density, the sibling-checkout dependency graph, and the single-maintainer velocity are the real risks.

If I were you I'd pick **one** structural simplification in the next quarter — most likely splitting `coder-tools` and `executor` along their natural module boundaries that the layer rules already want — and let everything else keep moving at its current cadence. The failure-modes doc will keep catching the rest.

---

Refresh done. 279 new commits since my last look (was ~30/day, now ~140/day — the new mutation-hardening campaign is what's pushing the volume). Pulled origin/main, re-ran the key metrics. Here's what changed and what didn't.

## Headline numbers

| | before | now | delta |
|---|---:|---:|---|
| Crates | 51 | **52** | +1 (`liberado-provider-free-proxy`) |
| Rust LOC | 205,661 | **237,555** | +16% |
| `.rs` files | 402 | **583** | +45% (test-sibling pattern) |
| `unwrap()` in non-test | 3,056 | **2,315** | **−24%** |
| `.expect(` in non-test | 712 | **411** | **−42%** |
| Combined panic points | 3,768 | **2,726** | **−28%** |
| Markdown files | 173 | **233** | +35% |
| Tests (using `#[test]` + `#[tokio::test]`) | 4,909 | **4,909** | flat (test count same, but split across more files) |

The unwrap drop is the most important number — and it's real, not test-extraction sleight-of-hand. You've done the actual work.

## New strengths

**1. The mutation-campaign ledger is best-in-class.** `mutants-ledger.json` is a committed, structured record of per-crate mutation-test results with `viable / caught / survived / timeout` counts, dated and re-runnable. The `Skills/mutants-campaign.md` playbook turns "fix the survivors" into a procedure with explicit gotchas (`minimum_test_timeout`, the scratch-copy-not-`git-checkout` rule, the "assert old text was present before replacing" rule). The cratest at 0% miss (`memory-store`, `scratchpad`, `notify`) prove the discipline works; the ones still at 30-70% (`server` 72.8%, `liberado-commands` 53.8%, `coder-agent` 45.7%) prove the work isn't finished — but at least you know exactly which surfaces are unverified. Very few projects have this.

**2. `liberado-provider-free-proxy` is exemplary.** 17 files, largest is `providers.rs` at 622 lines, zero god-modules. The lib doc justifies *why a network proxy* instead of an in-process `Provider` impl: *"a network seam cannot be bypassed by half the system."* This is the right architectural instinct — when the cost-of-bypass is non-zero for at least one composition root, you make the policy external. Compare to `coder-tools` `lib.rs` and the gap is obvious.

**3. Failure-modes doc is still growing — class 7 added 2026-08-09.** *"The setting that parses, validates, and is never read."* Then class 7b one level up: *"the config file was never loaded at all."* The taxonomy of bugs that shipped with green tests is now 8 classes with a meta-lesson. The fact that you keep adding to it is the right move.

**4. ~28% drop in production panic points.** This is the "safety is engineered, not prompted" pillar doing actual work. Combined with the `Skill` playbooks (`mutants-campaign.md`, `crap-harden-campaign.md`, `cold-review-pr.md`), the team-of-one has a procedure for everything that bites.

## New / updated weaknesses

**1. The god-crate story is half-told.** You split `coder-tools` to extract `hashline.rs` (1,267) and `repo_map.rs` (1,282) — but `lib.rs` is still **5,887 lines** with **20 tool definitions** in one file. The `coder-agent` `lib.rs` is **3,069 lines** with 19 `mod` declarations and a `ploc` waiver of 2,539. These are not anti-patterns because of waivers — they're anti-patterns because the waivers exist *because* the file is monolithic. The `module-health.toml` comment is admirably honest: *"Reasons that read as laziness get the whole contribution pushed back for rework."* But the count of waivers is now 13+, and the same `#[cfg(test)] #[path]` reason appears in many of them. That reason is "we keep growing the god-crate and need to waive the test growth that came with it."

**2. Mutation-test "one test per mutant" is coverage theatre, not test design.** You have **50 `*_survivor_tests.rs` files, 11,446 lines**. `main-agent/src/sessions/tests.rs` is **3,822 lines with 175 functions** (waiver: *"One `#[tokio::test]` per mutant killed; function count tracks campaign coverage by design."*). The campaign is real and the numbers are honest, but the resulting tests aren't behavioral — they're enumeration. When the system changes behavior, these tests don't get updated, they get new siblings. The mutation rate drops, the behavioral coverage doesn't.

**3. Gates are getting negotiated.** Two CI relaxations in the same window: CRAP per-function regressions below score 10 are now ignored (commit `0b84da42`: *"a 4 to 5 move is noise"*), and the ceiling was lowered from 450 to 150. Each one is *individually* defensible — the noise floor is real, 450 was too high — but together with the waiver expansion, the visible pattern is: ratchets that bite get softened. That's how ratchets die. Not dramatically, but in 5-point increments.

**4. The free-proxy crate is a tell.** When the newest crate is the most well-modularized one in the repo, the inference is that the older crates *could* be — they just didn't pay down the cost when they were young. Worth naming.

**5. Bus factor unchanged.** Still 1. 1,432 commits, 1 primary author.

## What didn't change

- Sibling-checkout fragility (turbovault + turbomcp path deps). Untouched.
- `[patch.crates-io]` overrides still in place. Comment still says "remove once turbomcp publishes the change."
- Daemon-only deploy artifact. Single point of failure for the homelab story.
- "Good-enough-and-integrated, not a Claude Code rival" positioning.

## Bottom line

The repo got *materially* better, not just bigger. The unwrap reduction and the mutation-ledger are the receipts. The honest counters are: the god-crates are still there with growing waiver stacks, and the test strategy is now mutation-driven to a degree that risks losing behavioral coverage. The free-proxy crate is the template — if `coder-tools` and `coder-agent` ever look like that, the waivers become unnecessary.

---

## Architecture consult (Sol / gpt-5.6-sol, corrected at `06aed987`)

_Appended 2026-08-27 after PR #213 merge and green main CI. Source: Codex Sol consult saved at the time as `sol-consult-out.md`. This section corrects stale audit claims (path-sibling fragility closed by #213) and ranks five architecture paydowns plus the canonical next backlog PR (0.7 / C3)._

**Scope.** This consult compares `docs/future-work/research/minimax_m3_audit.md` with the current
tree, the active backlog, and PR #213. It does not propose implementation now. The key distinction
is between a large file, a large crate, and a bad dependency boundary. The audit often treats these
as the same problem. They are not.

## Executive finding

The audit is strongest when it identifies Liberado's durable assets: mechanical layer rules,
mutation evidence, failure-mode records, provenance, and a domain-neutral executor with a second
domain proof. Its strongest remaining criticism is also correct: several high-change composition
files are too large for safe review.

The audit is stale on dependency mechanics. PR #213 (`a3d67694`, implementation commit
`440d5bcd`) replaced TurboVault and TurboMCP path siblings with git+tag dependencies at
`liberado-2026-08-27`. The lockfile resolves TurboVault at `bf9f0baf` and TurboMCP at `d5d9a9f8`.
A clean checkout no longer needs nested clones or their moving branches. This removes onboarding
and CI coupling to local sibling state.

It does **not** make Liberado independent of the forks. The workspace still patches TurboMCP crates
to the tagged fork because the registry release lacks request `_meta` pass-through. TurboVault also
declares registry TurboMCP versions, so the patch must keep one compatible TurboMCP type graph.
The correct current statement is: **reproducible fork pins, no path siblings, crates.io still
insufficient**. There should be no Epistates upstream PR work now.

## 1. What the audit gets right, and what is stale

### Right and still material

1. **The engineering culture is an architectural asset.** The failure-mode catalogue, executed
   defect mutations, layer-rule test, locked metadata check, and per-function CRAP ratchet turn
   design claims into checks. This is more valuable than a simple test count.

2. **The large composition files remain real review debt.** Current physical line counts are
   `coder-tools/src/lib.rs` 5,887, `executor/src/lib.rs` 5,279, `orchestrator/src/lib.rs` 3,266,
   and `coder-agent/src/lib.rs` 3,069. The module-health file confirms that production logical-line
   ceilings are waived for `executor/src/lib.rs` at 4,056 and `coder-agent/src/lib.rs` at 2,539.
   These are not merely crate-size complaints. `executor/src/lib.rs` combines public runtime
   contracts, task/report types, turn-loop control, loop guards, spill handling, reporting, and
   argument-similarity algorithms. `coder-tools/src/lib.rs` combines the runtime, the tool
   catalogue, dispatch, policy checks, filesystem walking, symbol extraction, and output shaping.
   A reviewer must hold too many independent invariants at once.

3. **Single-maintainer concentration remains true.** Current shortlog attributes 983 commits to
   the primary identity, 230 more to two ForrestThump identities, and 239 to the comparison bot.
   This is a continuity and independent-review risk. Commit velocity alone does not prove poor
   review, but it increases the value of small PRs and mechanical checks.

4. **Mutation hardening created a maintainability cost worth managing.** The tree has 50
   `*_survivor_tests.rs` files and 11,446 lines in them. `main-agent/src/sessions/tests.rs` has a
   waiver ceiling of 175 functions; `daemon/src/tests.rs` has 125. These tests have real value:
   they caught actual survivors. The debt is discoverability and duplicated fixtures, not proof
   that the tests are fake.

5. **Fail-closed configuration remains the correct design rule.** The historical optional-zone,
   unread-setting, and unloaded-config failures justify mechanical composition tests. The audit is
   right to treat silent safety disablement as more serious than a normal configuration bug.

### Stale, weak, or overstated

1. **Path-sibling fragility is closed.** PR #213 removed sibling checkout steps from all CI jobs and
   changed the workspace dependencies and lockfile to immutable tag resolutions. The audit's claim
   that the workspace requires two local checkouts at unpinned refs is now false.

2. **Fork dependence remains, but has a narrower shape.** `Cargo.toml` lines 103-132 show git+tag
   pins and a four-crate `[patch.crates-io]`. This is release and supply-chain debt, not local
   workspace topology debt. Do not reopen path-sibling machinery.

3. **The panic-point totals are snapshots, not a usable risk register.** The audit reports 3,768,
   then 2,726, but does not define a stable production-code classifier. An `unwrap` in a proven
   invariant is not equal to one on I/O or agent-controlled data. The reduction is encouraging;
   the raw total cannot rank the next work. A useful follow-up would classify panic sites by input
   trust and process blast radius, then ratchet only the unsafe classes.

4. **“Mutation coverage theatre” is too broad.** A test added for one mutant can still pin a public
   behavior. The factual concern is the 11,446-line survivor-test estate and the large-file
   waivers. Review test intent, fixture cost, and observable assertions. Do not delete evidence
   because of its origin.

5. **“Ratchets that bite get softened” is not supported by the cited changes.** Lowering the CRAP
   ceiling from 450 to 150 is stronger. Ignoring regressions while the current score remains below
   10 is an explicit noise floor, not removal of the per-function ratchet. Waiver growth is a valid
   warning, but it is separate evidence and should be tracked as such.

6. **“God-crate” is the wrong unit for most of the finding.** `coder-tools` and `coder-agent` are
   legitimate pack crates; `executor` is a legitimate kernel crate. Creating more crates would add
   public boundaries and layer pressure without necessarily reducing cognitive load. First split
   cohesive internal modules. Extract a crate only when reuse or dependency direction requires it.

7. **The numerical snapshot is already drifting.** The current tree still has 583 Rust files and
   has 231 Markdown files, not 233. Exact counts add little unless the method is committed and the
   metric drives a gate.

## 2. Top five debt paydowns by leverage

These are architecture paydowns, not permission to bypass the active backlog. Each should be one
small PR after the measurement work that conflicts with it.

| Rank | Debt and leverage | Owner / crates | One-PR shape | Conflict and sequencing |
|---:|---|---|---|---|
| **1** | **Split the executor control plane into internal modules.** This has the highest leverage because every direct, subagent, and domain-pack execution crosses this kernel. Smaller review units reduce risk in budgets, loop termination, report gating, and tool invocation without changing the public waist. | **Liberado**: `crates/executor`; consumers in `orchestrator`, `coder-agent`, and session code should not change. | Move one cohesive family first: loop-guard detection and escalation (`ArgMatch`, `LoopProfile`, repetition/cycle/similarity helpers) into `loop_guard.rs`. Preserve public re-exports and behavior. Move its existing tests with it. No new crate and no API redesign. Remove or lower only the affected waiver if the first slice makes that honest. | High conflict with any executor or completion-gate implementation. Do it **after C5**, not during the controlled comparison or gate experiment. It should not conflict with E4/E5. |
| **2** | **Split coding-tool catalogue, dispatch, and algorithms.** The 5,887-line file is a pack-level hotspot. Tool schema, permission checks, execution, and text/index helpers now change in one review unit. Separation also makes A2 changes attributable. | **Liberado**: `crates/coder-tools`; keep the `ToolRuntime` contract in `executor`. | After A1/A2, move one stable tool family and its argument types/handlers into a private module, with the root retaining catalogue assembly and re-exports. A good first slice is read/search/list tooling; do not redesign schemas or prompts in the move. | Direct conflict with **A2**, which may change the catalogue path. A2 must land first. Also avoid overlap with deferred C6 repository-map work. |
| **3** | **Converge the dependency boundary on published releases.** PR #213 made builds reproducible, but the fork tag and `[patch.crates-io]` remain a permanent release exception if they have no exit test. This affects every build and supply-chain review. | **TurboMCP first**, then **TurboVault**, then a small **Liberado** cleanup. No Epistates PR now. | This is not one mixed PR. (a) TurboMCP publishes the required `_meta` behavior and compatible crate set. (b) TurboVault consumes that release and publishes its compatible set. (c) Liberado replaces git pins and removes the patch in one lockfile PR, with provenance e2e and `cargo metadata --locked` evidence. | Externally blocked today; crates.io is insufficient. It overlaps dependency manifests and supply-chain policy, but not C3. E5 may change the TurboMCP release contents; coordinate releases rather than publish twice. |
| **4** | **Refactor mutation tests around behavioral fixtures without reducing killed-mutant evidence.** The risk is not the number of tests. It is that 11,446 survivor-test lines and files with 175/125 functions make intent hard to find and fixture changes broad. | **Liberado**: start with either `crates/main-agent/src/sessions/tests.rs` or `crates/daemon/src/tests.rs`, never both in one PR. | Extract shared fixture/builders and group tests by public state transition. Keep each survivor caught; run a small representative mutation set before and after. The success condition is less duplicated setup and a lower file-health waiver, not fewer assertions. | High conflict with active mutation campaigns and daemon CI repair. Wait for main CI and the current campaign to settle. Independent of TurboVault/TurboMCP. |
| **5** | **Make configuration arrival a composition contract.** Existing literal rules catch some hardcoded consumers, but the historical failures crossed resolution, deserialization, assembly, and runtime use. This is high leverage because a silent default can disable a safety feature across every surface. | **Liberado**: `crates/config`, `config-loader`, `test-support`, and the relevant composition root only. Do not move policy into TurboVault. | Add one table-driven test that selects a safety-critical `CoderTuning` field, loads it through the real config resolution path, assembles the run config, and observes the changed runtime value. Extend the table one field at a time; do not add another config framework. | Likely conflict with new tuning fields and ACP/config work. It should follow C5 and A2 if those change configuration. No conflict with E4; only indirect conflict with E5 deployment config. |

### Liberado versus TurboVault/TurboMCP boundary

- Liberado owns session semantics, executor policy, tool selection, configuration arrival, inbox
  semantics, provenance use, and surface behavior.
- TurboVault owns positive vault directory enumeration and vault storage/query primitives. E4
  belongs there. Liberado must not reproduce vault traversal to avoid the dependency.
- TurboMCP owns HTTP/SSE transport reliability and protocol metadata carriage. E5 and `_meta`
  pass-through belong there. Liberado may test these contracts but should not fork transport logic
  into its MCP adapter.
- The git tags are acceptable interim integration artifacts. They are not a reason to put Liberado
  policy into the forks, and they are not equivalent to published dependency closure.

## 3. Non-goals

- Do not restore nested TurboVault or TurboMCP clones, path dependencies, sibling checkout actions,
  or moving-branch CI.
- Do not open Epistates upstream PRs now. The current controlled forks are the integration point.
- Do not split `executor`, `coder-tools`, or `coder-agent` into new crates only to reduce file counts.
  Preserve the layer model and first use private modules.
- Do not run a broad `unwrap`/`expect` removal campaign from the audit's raw count. Classify unsafe
  sites and fix agent-controlled, I/O, and daemon-fatal paths first when evidence selects them.
- Do not remove mutation tests or weaken CRAP/module-health gates to make refactors easy. Preserve
  the mutations they catch and lower waivers when structure improves.
- Do not narrow tools, change prompts, change `max_turns`, or enable the completion gate before the
  backlog measurements support those changes.
- Do not combine E4, E5, E2, or dependency-release convergence in one cross-repository change.
- Do not prioritize TUI/WebUI/Telegram consolidation from this audit. It gives no usage or cost
  evidence that a surface should be removed.
- Do not start deferred C6 repository-map work. The active backlog explicitly says focused search
  is sufficient until measurement proves a context-selection limit.

## 4. One next PR after main CI is green

**Do backlog item 0.7 / C3: publish the controlled cross-harness baseline.** This is the only valid
next PR. The active, canonical backlog says to take the first open unblocked item, and C3 is first.
It is a report PR, not another harness or architecture change.

PR shape:

1. Pin Liberado, Pi, Hermes, and Deep Agents to recorded versions and one repository commit.
2. Use the same task, model, provider, sampling settings, and resource limits. Keep native prompts
   and tool schemas.
3. Report ship-gate and merge-ready rate, cost per accepted result, duration percentiles only where
   sample size permits them, human repair, and trace-linked failure classes.
4. Run repeats where cost permits. If Hermes is absent, label the result evidence, not the baseline.
5. Do not rank harnesses, change `max_turns`, enable the completion gate, narrow tools, or mix in any
   of the five structural paydowns above.

**Conflict shape:** branch from the final CI-green main commit because the measured artifact and
ship bar must match the reported revision. The PR should primarily touch the dated findings/report
and any run receipts required by the comparison specification. It should not touch `executor`,
`coder-tools`, TurboVault, TurboMCP, or workspace dependency manifests. C5 depends on this report
producing a non-zero finish rate; A1 is later and independent measurement work.

This sequence is the sharp choice: establish whether the coding system finishes accepted work,
then measure the completion gate, then measure tool economics. Structural cleanup before those
measurements creates conflicts and changes the artifact that the backlog is trying to understand.
