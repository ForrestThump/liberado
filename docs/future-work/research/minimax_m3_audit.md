Hold on — main hasn't moved. I'm sitting on `3c54be8c` (the Sol/Minimax paydowns merge from my last audit), and `git fetch` shows `HEAD..origin/main` = 0 commits behind. Whatever improvements you're thinking of are on branches, not on main.

What I can see on remotes:

- **`origin/analyze-repository-antipatterns`** — 1 commit ahead: *"doc: antipattern analysis report"*. Looks like a writeup, possibly responding to my prior audit.
- **`origin/feat/crap-under-75`** — substantial work, multi-commit. Ratcheting CRAP ceiling down to 75 across `cli/mutants-cmd`, `tui`, `test-support`, `server/goals`, `coder-tools`, `eval`, plus baseline refreshes. This is a real quality push, not docs.
- **`origin/feat/delegate-d2-sse`** — force-updated; has new commits I haven't seen.
- **`origin/feat/mutants-campaign-ledger`** — name suggests the ledger tooling.
- **`origin/chat-client-contract-campaign`**, **`origin/fix/acp-bridge-mutants`** — campaign branches.

So: are you asking me to audit a specific branch, did you mean to push to main, or did the merge just not happen yet? I'd rather look at the right thing than write a fourth audit on the same tree I already audited.

Sure. Five picks, ordered by what I think moves the needle most. One honest caveat up front: I've been working from inspection of the tree, not from running it — so these are based on the artifacts I can see, not on your day-to-day. Tell me where my priors are wrong.

## 1. Classify the unwraps, don't just count them

You're right that this is on the list — but I'd push back on the framing. The current 2,579 production unwraps is a *snapshot*, not a *risk register*. Sol caught me on this last round and they were correct. A 6-week "remove unwraps" campaign is going to churn a lot of code without moving the dial on the actual failure modes that bite.

The leverage is in the *classifier*. Sort every production unwrap into three buckets:

- **Proven invariant** — `unwrap` on a regex you just compiled, a literal you just matched against, an index you just bounds-checked. Cheap, correct, no change needed.
- **Local failure** — `unwrap` on a value that flows through a function boundary, the function returns `Result`, the caller propagates. Fix with `?`. Low effort, real value.
- **Process-fatal / agent-controlled I/O** — `unwrap` on JSON parsed from the model, on a path string the executor accepts, on a write that *should* have errored back to the caller. These are the ones that take down the daemon at 3am.

The ratchet targets the third class only. The first class is fine and shouldn't move. The second class is bounded and ratchets naturally. I'd expect a 60-80% drop in the *unsafe* class with maybe 5% growth in the *proven* class — and you'd have an actual risk number, not a count.

## 2. Converge the fork dep on a published release

This is the single biggest contributor-experience hazard that the git+tag pin didn't actually fix. It changed the *shape* of the dependency — from "clone siblings at HEAD" to "trust a personal fork holds a tag forever" — but it's still a personal fork. The `[patch.crates-io]` redirects are still there. The `_meta` pass-through is still fork-only. Every build is a snapshot of `ForrestThump/{turbovault,turbomcp}` at `liberado-2026-08-27`.

The exit path is well-defined: TurboMCP publishes the `_meta` behavior → TurboVault consumes that release → Liberado drops the patch in one lockfile PR. That's a 3-repo coordinated release. It's not glamorous but it's the highest leverage because everything else (build reproducibility, supply-chain review, second-user onboarding) gets easier once the patch is gone.

I'd put this second because it's upstream of the rest of the dev experience. The path-sibling retirement last month bought time; this is the next step.

## 3. Audit every optional config gate for "default = refuse"

Class 2 (the guard that was off by default), class 7 (the setting that parses and is never read), and class 7b (the config file that was never loaded) all share one root cause: *an optional thing that should have been required, defaulted to permissive*. The CoderTuning arrival contract fixed one instance. There are likely more.

The mechanical pass: list every `Option<T>` in the config types, every optional config file, every default that produces a permissive value, and ask *"if a user never touches this, does the safe path run or the unsafe path?"* Add an arrival test for each safe-default case, exactly like the CoderTuning contract. This is the same pattern as the failure-modes taxonomy — name the class, find instances, fix them, add a check.

The reason this is high-leverage is that every one of those classes *shipped with a green test suite*. The class 2 guard was off for *months*. The class 7b config file wasn't loaded at all. A single audit pass closes a whole class of future bugs.

## 4. Behavioral / arrival tests for the safety-critical paths

The CoderTuning contract is a great seed. The general version is: *for every safety-critical code path, there should be a test that exercises the same wire the user runs* — real config resolution, real assembly, real runtime path — not the unit-test double, not the MockProvider. The mutation campaign proves the code does what it says; the arrival test proves the system does what the user needs.

The reason this matters: the failure-modes doc is full of bugs that survived mutation testing and only showed up in live use. *"The guard that was off by default"* passed every unit test because the unit test instantiated the guard *with* the option turned on. A behavioral / arrival test would have started from a config file and watched the guard stay off.

The lift is modest — most of the patterns exist in `liberado-coder-agent` already, and the CoderTuning test is the template. The payoff is converting a class of "shipped-with-green-tests" bugs into caught-at-CI bugs.

## 5. Next god-file splits: daemon tests + coder-agent lib

The sessions test refactor (3,822 lines / 175 functions → 38 lines + 11 submodules, 4 waivers deleted) is the proven template. The next two targets:

- **`crates/daemon/src/tests.rs`** — 4,496 lines, 4 waivers, the largest waived test file in the tree. Same play: shared fixtures, group by state transition, drop the waivers.
- **`crates/coder-agent/src/lib.rs`** — 3,069 lines, `ploc` waiver 2,539. The 19 `mod` declarations are already there; the body still has a lot in it. After daemon tests, this is the next-biggest review-unit win.

I'd do daemon tests first because it's a test file, which means the refactor is mechanical and the failure mode is "tests still pass." The coder-agent lib split is more load-bearing.

---

The one I'd add if I had six: **keep the bus-factor experiment going.** Cursor Agent has 4 commits in the last batch, and the dogfooding documentation is good. The single-maintainer concentration is the thing nothing on this list touches. You're already moving on it; just don't let it slip below the visible-line items.