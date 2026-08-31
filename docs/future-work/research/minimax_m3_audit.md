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

User:

Do you think it would be worth making an automated CI check to enforce unwrap() classification and justification? Basically it could parse every unwrap call, see if there's an associated classification (a comment on the same line with some marker like a hashtag), and justification if needed. Then if it regresses, it fails CI. Something like this may already exist ... but if not, it would be easy to build.


Minimax:

Honest take: the instinct is good, but the specific implementation is the wrong shape, and I think you'd build something worse than what's already in the tree. Let me push back on three things.

**1. The "hashtag on the line above" pattern is weaker than your existing waiver pattern.**

Look at `module-health.toml` — your waivers are *typed artifacts*:

```toml
[[waiver]]
path = "crates/coder-agent/src/lib.rs"
metric = "ploc"
ceiling = 2539
reason = "Same as fanout.rs: three lines of #[cfg(test)] #[path] sibling wiring (lib_survivor_tests.rs); the crate-root module declaration cannot live anywhere else."
reviewed_on = "2026-08-24"
```

That's path, metric, ceiling, reason, *date*. It's grep-friendly, review-friendly, version-controlled, and survives the file being moved. A line-comment hashtag is a single point of context that disappears the moment someone reorders code, refactors the function, or moves the unwrap to a helper. Worse, it's trivially gameable: `// #safe` on every line takes 2 seconds per call site and the gate goes green without anyone thinking. The repo's culture is "name the metric, justify the exception, date it." A hashtag is a regression to "trust me."

**2. Clippy already does most of this for free.**

`clippy::unwrap_used` and `clippy::expect_used` exist as deny-by-default lints. Your `just check` already runs `cargo clippy ... -D warnings` — adding `-D clippy::unwrap_used -D clippy::expect_used` is a one-line change. Clippy gives you the *parse* for free, the *source location* for free, and the *CI integration* for free. What's left is just the classification, which is the actual value.

**3. The "easy to build" framing is a red flag here.**

The easy part is a regex that finds `.unwrap()`. The hard part is everything else:
- Correctly handling macro-generated unwraps (e.g., `tokio::main` expansions, `derive_more`, the `?` shorthand)
- Resolving source locations through `mod` boundaries
- Distinguishing `Result::unwrap` from `Option::unwrap` and the variant of each
- Handling unwraps inside `const fn` and trait default methods
- Integrating with the existing CRAP / mutation / module-health pipeline without re-inventing the baseline dance

You'd need `syn` or `ra_ap_syntax` (which your repo already uses via `tree-sitter` for some things — but check the import cost). That's a non-trivial dependency for a tool whose only job is to be a comment-parsing gate.

## What I'd actually do

The repo's discipline is "measure first, then ratchet." The `mutants-campaign` skill is exactly this: run the analysis, see the distribution, then write the gate. You don't write a ratchet against a number you haven't classified yet. So:

**Step 1: Write a one-shot classifier, not a gate.** A script (or a `cargo` subcommand in `liberado-cli`) that walks every `unwrap()` and `expect()` in `crates/**/src/**` (excluding tests), and emits a JSON report with: file, line, expression type (`Result` vs `Option`), the inferred context (inside a function returning `Result`? in a `main`? in a `tokio::spawn` block?), and a suggested bucket (proven invariant / local failure / process-fatal). Run it. Look at the output. I bet the distribution tells you something you didn't expect — probably that the *proven invariant* class is way larger than the *process-fatal* class, in which case a ratchet against the latter is much more aggressive than against the total.

**Step 2: Once you have the data, extend `module-health.toml` (or add a sibling `unwrap-classification.toml`) with the same `path / metric / ceiling / reason / reviewed_on` shape.** Per-file ceilings for the `process_fatal` class. The ratchet says "this file has N fatal-class unwraps; it must not grow; new code must convert to `?`." That's a number with a *meaning*, not a hashtag.

**Step 3: Optionally, turn on `clippy::unwrap_used` as deny-by-default *for new code only*.** Clippy has `#[allow(clippy::unwrap_used)]` per-function, and you can drive a baseline of allowed sites the same way you drive `crap-baseline.json` — a committed JSON of permitted call sites, and any new one fails CI. The waiver for an existing site is the typed `unwrap-classification.toml` entry, not a comment.

**Step 4: Don't build a comment parser.** The hashtag idea is the kind of tool that looks like progress on a dashboard and produces no actual safety improvement. The classification is the *thinking* — that's the part that takes 6 weeks, not the parsing.

The reason I'm pushing back is that I've watched a lot of repos add comment-driven lint tools and they all do the same thing: developers add the marker because the build is red, the marker says nothing, and a year later the lint is in the way of a refactor and gets deleted. The repo you have is one of the few where I'd expect the discipline to *actually work* — but only if the tool matches the existing culture. The culture here is structured waivers with review dates. Lean into that.

If you want, I can sketch the `unwrap-classification.toml` schema and the report-script stub to make it concrete. But I'd start by running the classifier once on the current tree and seeing what the actual distribution looks like before committing to any ratchet shape.