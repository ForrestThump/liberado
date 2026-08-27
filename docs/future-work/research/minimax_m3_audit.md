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

